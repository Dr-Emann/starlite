use pyo3::prelude::*;
use pyo3::types::{PyMapping, PyType};
use pyo3::{PyTraverseError, PyVisit};

pyo3::import_exception!(starlite.exceptions, ImproperlyConfiguredException);
pyo3::import_exception!(starlite.exceptions, MethodNotAllowedException);
pyo3::import_exception!(starlite.exceptions, NotFoundException);

pub type ASGIApp = PyAny;

#[derive(Debug, FromPyObject)]
pub struct StarliteApp {
    static_paths: Py<PyAny>,
    build_route_middleware_stack: Py<PyAny>,
}

impl StarliteApp {
    pub fn path_in_static(&self, py: Python<'_>, path: &str) -> PyResult<bool> {
        self.static_paths.as_ref(py).contains(path)
    }

    pub fn build_route(&self, route: Route, handler: &PyAny) -> PyResult<Py<PyAny>> {
        let py = route.0.py();
        self.build_route_middleware_stack
            .call1(py, (route.0, handler))
    }

    pub fn visit_python(&self, visit: &PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.static_paths)?;
        visit.call(&self.build_route_middleware_stack)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct RouteTypes {
    http: Py<PyType>,
    websocket: Py<PyType>,
    asgi: Py<PyType>,
}

impl RouteTypes {
    pub fn new(py: Python<'_>) -> PyResult<Self> {
        let module = py.import("starlite.routes")?;
        let extract_type = |name: &str| -> PyResult<Py<PyType>> {
            let any: &PyAny = module.getattr(name)?;
            Ok(any.downcast::<PyType>()?.into())
        };
        Ok(RouteTypes {
            http: extract_type("HTTPRoute")?,
            websocket: extract_type("WebSocketRoute")?,
            asgi: extract_type("ASGIRoute")?,
        })
    }

    pub fn is_http(&self, route: Route) -> PyResult<bool> {
        route.0.is_instance(self.http.as_ref(route.0.py()))
    }

    pub fn is_websocket(&self, route: Route) -> PyResult<bool> {
        route.0.is_instance(self.websocket.as_ref(route.0.py()))
    }

    pub fn is_asgi(&self, route: Route) -> PyResult<bool> {
        route.0.is_instance(self.asgi.as_ref(route.0.py()))
    }

    pub fn visit_python(&self, visit: &PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.http)?;
        visit.call(&self.websocket)?;
        visit.call(&self.asgi)?;
        Ok(())
    }
}

#[derive(Debug, Copy, Clone, FromPyObject)]
pub struct Route<'a>(&'a PyAny);

impl<'a> Route<'a> {
    pub fn path(&self) -> PyResult<&'a str> {
        self.0.getattr("path")?.extract()
    }

    pub fn path_parameters(&self) -> PyResult<&'a PyAny> {
        self.0.getattr("path_parameters")
    }

    pub fn handler(&self) -> PyResult<&'a PyAny> {
        self.0.getattr("route_handler")
    }

    pub fn http_handlers(&self) -> PyResult<impl Iterator<Item = PyResult<(&'a str, &'a PyAny)>>> {
        let mapping: &PyMapping = self.0.getattr("route_handler_map")?.downcast()?;
        let iter = mapping.items()?.iter()?;
        Ok(iter.map(|item| -> PyResult<(&'a str, &'a PyAny)> {
            let item = item?;
            let (name, (handler, _)): (&str, (&PyAny, &PyAny)) = item.extract()?;
            Ok((name, handler))
        }))
    }

    pub fn type_name(&self) -> PyResult<&'a str> {
        self.0.get_type().name()
    }
}
