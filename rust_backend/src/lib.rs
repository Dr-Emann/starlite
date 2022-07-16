mod wrappers;

use pyo3::exceptions::PyTypeError;
use pyo3::gc::{PyTraverseError, PyVisit};
use pyo3::prelude::*;
use pyo3::types::{PyList, PyMapping, PySequence};
use std::mem;

use wrappers::{ASGIApp, RouteTypes, StarliteApp};

type HashMap<K, V> = std::collections::HashMap<K, V, ahash::RandomState>;
type HashSet<K> = std::collections::HashSet<K, ahash::RandomState>;

#[pyclass]
#[derive(Debug)]
struct RouteMap {
    app: StarliteApp,
    route_types: RouteTypes,
    path_param_parser: Py<PyAny>,
    param_routes: Node,
    plain_routes: HashMap<String, Leaf>,
}

#[derive(Debug, Default)]
struct Node {
    children: HashMap<String, Node>,
    placeholder_child: Option<Box<Node>>,
    leaf: Option<Leaf>,
}

#[derive(Debug)]
struct Leaf {
    is_asgi: bool,
    static_path: Option<String>,
    path_parameters: Py<PyAny>,
    asgi_handlers: HashMap<HandlerType, Py<ASGIApp>>,
}

impl Leaf {
    fn new(params: Py<PyAny>) -> Self {
        Self {
            path_parameters: params,
            asgi_handlers: Default::default(),
            is_asgi: false,
            static_path: None,
        }
    }

    fn traverse_python_objects(&self, visit: &PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.path_parameters)?;
        for handler in self.asgi_handlers.values() {
            visit.call(handler)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum HandlerType {
    Asgi,
    Websocket,
    // HTTP methods taken from starlite.types.Method
    HttpGet,
    HttpPost,
    HttpDelete,
    HttpPatch,
    HttpPut,
    HttpHead,
    HttpOther(String),
}

impl HandlerType {
    fn from_http_method(method: &str) -> Self {
        match method {
            "GET" => Self::HttpGet,
            "POST" => Self::HttpPost,
            "DELETE" => Self::HttpDelete,
            "PATCH" => Self::HttpPatch,
            "PUT" => Self::HttpPut,
            "HEAD" => Self::HttpHead,
            _ => Self::HttpOther(String::from(method)),
        }
    }
}

fn split_path(path: &str) -> impl Iterator<Item = &'_ str> {
    path.split('/').filter(|s| !s.is_empty())
}

fn build_param_set<'a>(
    path_parameters: &[&'a PyAny],
    param_strings: &mut HashSet<&'a str>,
) -> PyResult<()> {
    param_strings.clear();
    param_strings.reserve(path_parameters.len());
    for &path_param in path_parameters {
        let full_name: &str = path_param
            .get_item(pyo3::intern!(path_param.py(), "full"))?
            .extract()?;
        param_strings.insert(full_name);
    }
    Ok(())
}

impl RouteMap {
    fn add_routes_(&mut self, items: &PySequence) -> PyResult<()> {
        let p = items.py();
        let mut param_strings = HashSet::default();
        for route in items.iter()? {
            let route: wrappers::Route<'_> = route?.extract()?;
            let path = route.path()?;
            let path_parameters = route.path_parameters()?;
            let path_parameters_vec: Vec<&PyAny> = path_parameters.extract()?;

            let in_static = self.app.path_in_static(p, path)?;
            let leaf: &mut Leaf = if !path_parameters_vec.is_empty() || in_static {
                build_param_set(&path_parameters_vec, &mut param_strings)?;

                let mut node = &mut self.param_routes;
                for s in split_path(path) {
                    // Could we just assume a path segment that starts and ends
                    // with `{}` is a placeholder?
                    let is_placeholder = s.starts_with('{')
                        && s.ends_with('}')
                        && param_strings.contains(&s[1..s.len() - 1]);

                    node = if is_placeholder {
                        node.placeholder_child.get_or_insert_with(Default::default)
                    } else {
                        node.children
                            .entry(String::from(s))
                            .or_insert_with(Default::default)
                    };
                }
                // Found where the leaf should be, get it, or add a new one
                node.leaf
                    .get_or_insert_with(|| Leaf::new(path_parameters.into()))
            } else {
                self.plain_routes
                    .entry(String::from(path))
                    .or_insert_with(|| Leaf::new(path_parameters.into()))
            };
            if path_parameters.ne(&leaf.path_parameters)? {
                return Err(wrappers::ImproperlyConfiguredException::new_err(
                    "Routes with conflicting path parameters",
                ));
            }
            if in_static {
                leaf.is_asgi = true;
                leaf.static_path = Some(String::from(path));
            }

            let route_types = &self.route_types;
            if route_types.is_http(route)? {
                for item in route.http_handlers()? {
                    let (method, handler) = item?;
                    leaf.asgi_handlers.insert(
                        HandlerType::from_http_method(method),
                        self.app.build_route(route.0, handler)?,
                    );
                }
            } else if route_types.is_websocket(route)? {
                leaf.asgi_handlers.insert(
                    HandlerType::Websocket,
                    self.app.build_route(route.0, route.handler()?)?,
                );
            } else if route_types.is_asgi(route)? {
                leaf.asgi_handlers.insert(
                    HandlerType::Asgi,
                    self.app.build_route(route.0, route.handler()?)?,
                );
                leaf.is_asgi = true;
            } else {
                let route_type_name = route.0.get_type().name()?;
                return Err(PyTypeError::new_err(format!(
                    "Unknown route type {route_type_name}"
                )));
            }
        }
        Ok(())
    }

    fn resolve_route_(&self, scope: &PyMapping) -> PyResult<Py<PyAny>> {
        let py = scope.py();
        let path: &str = scope.get_item(pyo3::intern!(py, "path"))?.extract()?;
        let mut path = path.strip_suffix(|ch| ch == '/').unwrap_or(path);
        if path.is_empty() {
            path = "/";
        }
        let (leaf, params) = match self.plain_routes.get(path) {
            Some(leaf) => (leaf, PyList::empty(py)),
            None => self.find_route(path, scope)?,
        };
        scope.set_item(
            pyo3::intern!(py, "path_params"),
            self.parse_path_params(leaf.path_parameters.as_ref(py), params)?,
        )?;

        let handler: Option<&Py<ASGIApp>> = if leaf.is_asgi {
            leaf.asgi_handlers.get(&HandlerType::Asgi)
        } else {
            let scope_type: &str = scope.get_item(pyo3::intern!(py, "type"))?.extract()?;
            if scope_type == "http" {
                let scope_method: &str = scope.get_item(pyo3::intern!(py, "method"))?.extract()?;
                let handler = leaf
                    .asgi_handlers
                    .get(&HandlerType::from_http_method(scope_method));
                if handler.is_none() {
                    return Err(wrappers::MethodNotAllowedException::new_err(()));
                }
                handler
            } else {
                leaf.asgi_handlers.get(&HandlerType::Websocket)
            }
        };
        let handler: Py<ASGIApp> = handler
            .ok_or_else(|| wrappers::NotFoundException::new_err(()))?
            .clone_ref(py);
        Ok(handler)
    }

    fn find_route<'a>(&'a self, path: &str, scope: &'a PyMapping) -> PyResult<(&Leaf, &PyList)> {
        let py = scope.py();
        let key_path = pyo3::intern!(py, "path");
        let mut params = Vec::new();
        let mut node = &self.param_routes;
        for component in split_path(path) {
            if let Some(child) = node.children.get(component) {
                node = child;
                continue;
            }
            if let Some(child) = &node.placeholder_child {
                node = child;
                params.push(component);
                continue;
            }
            let static_path = node
                .leaf
                .as_ref()
                .and_then(|leaf| leaf.static_path.as_deref());
            if let Some(static_path) = static_path {
                if static_path != "/" {
                    let old_scope_path: &str = scope.get_item(key_path)?.extract()?;
                    let new_scope_path = old_scope_path.replace(static_path, "");
                    scope.set_item(key_path, new_scope_path)?;
                }
                continue;
            }

            return Err(wrappers::NotFoundException::new_err(()));
        }
        let leaf = match &node.leaf {
            Some(leaf) => leaf,
            None => return Err(wrappers::NotFoundException::new_err(())),
        };
        let list = PyList::new(py, params);
        Ok((leaf, list))
    }

    fn parse_path_params(&self, params: &PyAny, values: &PyList) -> PyResult<Py<PyAny>> {
        self.path_param_parser.call1(params.py(), (params, values))
    }

    fn clear(&mut self) {
        let node = mem::take(&mut self.param_routes);
        let mut stack = Vec::new();
        stack.push(node);
        while let Some(mut node) = stack.pop() {
            if let Some(child) = node.placeholder_child.take() {
                stack.push(*child);
            }
            stack.extend(mem::take(&mut node.children).into_values());

            // Node no longer contains any child nodes, will be an empty drop
            drop(node);
        }
    }
}

impl Drop for RouteMap {
    // Avoid blowing the stack if the children get too deep
    fn drop(&mut self) {
        self.clear();
    }
}

#[pymethods]
impl RouteMap {
    #[new]
    fn new(py: Python<'_>, app: StarliteApp) -> PyResult<Self> {
        let module = py.import("starlite.parsers")?;
        let path_param_parser = module.getattr("parse_path_params")?.into();
        Ok(Self {
            app,
            route_types: RouteTypes::new(py)?,
            path_param_parser,
            param_routes: Node::default(),
            plain_routes: HashMap::default(),
        })
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        format!("{:#?}", self)
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        self.app.visit_python(&visit)?;
        self.route_types.visit_python(&visit)?;

        visit.call(&self.path_param_parser)?;

        for leaf in self.plain_routes.values() {
            leaf.traverse_python_objects(&visit)?;
        }

        let mut node_stack: Vec<&Node> = Vec::new();
        node_stack.push(&self.param_routes);

        while let Some(node) = node_stack.pop() {
            if let Some(leaf) = &node.leaf {
                leaf.traverse_python_objects(&visit)?;
            }

            if let Some(child) = &node.placeholder_child {
                node_stack.push(child);
            }
            node_stack.extend(node.children.values());
        }

        Ok(())
    }

    fn __clear__(&mut self) {
        self.clear();
    }

    /// Add an item
    #[pyo3(text_signature = "(routes)")]
    fn add_routes(&mut self, routes: &PySequence) -> PyResult<()> {
        self.add_routes_(routes)
    }

    #[pyo3(text_signature = "(scope)")]
    fn resolve_route(&self, scope: &PyMapping) -> PyResult<Py<PyAny>> {
        self.resolve_route_(scope)
    }
}

/// A Python module implemented in Rust.
#[pymodule]
fn rust_backend(_p: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<RouteMap>()?;
    Ok(())
}
