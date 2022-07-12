from collections import defaultdict
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Collection, Dict, List, Optional, Tuple, cast

from starlite.enums import ScopeType
from starlite.exceptions import (
    ImproperlyConfiguredException,
    MethodNotAllowedException,
    NotFoundException,
)
from starlite.parsers import parse_path_params
from starlite.routes import ASGIRoute, BaseRoute, HTTPRoute, WebSocketRoute

if TYPE_CHECKING:  # pragma: no cover
    from starlette.types import ASGIApp, Scope

    from starlite.app import Starlite


@dataclass
class _RouteMapLeafData:
    path_parameters: List[Dict[str, Any]] = field(default_factory=list)
    asgi_handlers: Dict[str, "ASGIApp"] = field(default_factory=dict)
    is_asgi: bool = False
    static_path: str = ""


@dataclass
class _RouteMapTree:
    children: Dict[str, "_RouteMapTree"] = field(default_factory=lambda: defaultdict(_RouteMapTree))
    data: Optional[_RouteMapLeafData] = None


class PythonRouteMap:
    def __init__(self, app: "Starlite"):
        self._app = app
        self._plain_routes: Dict[str, _RouteMapTree] = defaultdict(_RouteMapTree)
        self._tree: _RouteMapTree = _RouteMapTree()

    def add_routes(self, routes: Collection[BaseRoute]) -> None:
        """
        Add routes to this map
        """
        for route in routes:
            path = route.path
            cur: _RouteMapTree
            if route.path_parameters or path in self._app.static_paths:
                for param_definition in route.path_parameters:
                    path = path.replace("{" + param_definition["full"] + "}", "*")
                cur = self._tree
                components = ["/", *[component for component in path.split("/") if component]]
                for component in components:
                    cur = cur.children[component]
            else:
                cur = self._plain_routes[path]

            if not cur.data:
                cur.data = _RouteMapLeafData()
            data = cur.data
            if data.path_parameters and data.path_parameters != route.path_parameters:
                raise ImproperlyConfiguredException("Routes with conflicting path parameters")
            data.path_parameters = route.path_parameters
            if path in self._app.static_paths:
                data.is_asgi = True
                data.static_path = path
            # TODO: Check if there's already something at `cur.asgi_handlers[x]`
            if isinstance(route, HTTPRoute):
                for method, (handler, _) in route.route_handler_map.items():
                    data.asgi_handlers[method] = self._app.build_route_middleware_stack(route, handler)
            elif isinstance(route, WebSocketRoute):
                data.asgi_handlers[ScopeType.WEBSOCKET] = self._app.build_route_middleware_stack(
                    route, route.route_handler
                )
            elif isinstance(route, ASGIRoute):
                data.asgi_handlers[ScopeType.ASGI] = self._app.build_route_middleware_stack(route, route.route_handler)
                data.is_asgi = True
            else:
                raise ImproperlyConfiguredException(f"Unknown route type {type(route)})")

    def resolve_route(self, scope: "Scope") -> "ASGIApp":
        """
        Resolve the app for this scope
        """
        try:
            path = cast(str, scope["path"]).strip()
            if path != "/" and path.endswith("/"):
                path = path.rstrip("/")
            if path in self._plain_routes:
                cur = self._plain_routes[path]
                path_params: List[str] = []
            else:
                cur, path_params = self._traverse_route_map(path, scope)

            if not cur.data:
                raise NotFoundException()
            data = cur.data
            scope["path_params"] = parse_path_params(data.path_parameters, path_params)

            if data.is_asgi:
                return data.asgi_handlers[ScopeType.ASGI]
            if scope["type"] == ScopeType.HTTP:
                if scope["method"] not in data.asgi_handlers:
                    raise MethodNotAllowedException()
                return data.asgi_handlers[scope["method"]]
            return data.asgi_handlers[ScopeType.WEBSOCKET]
        except KeyError as e:
            raise NotFoundException() from e

    def _traverse_route_map(self, path: str, scope: "Scope") -> Tuple[_RouteMapTree, List[str]]:
        """
        Traverses the application route mapping and retrieves the correct node for the request url.

        Raises NotFoundException if no correlating node is found
        """
        path_params: List[str] = []
        cur = self._tree
        components = ["/", *[component for component in path.split("/") if component]]
        for component in components:
            # TODO: What if we try to match the requested path `/*` against a `@get("/{s:str}")`??
            if component in cur.children:
                cur = cur.children[component]
                continue
            if "*" in cur.children:
                path_params.append(component)
                cur = cur.children["*"]
                continue
            if cur.data and cur.data.static_path:
                if cur.data.static_path != "/":
                    scope["path"] = scope["path"].replace(cur.data.static_path, "")
                # TODO: This _feels_ like this should be a break?
                continue
            raise NotFoundException()
        return cur, path_params


try:
    # pylint: disable=useless-import-alias,unused-import
    # This tells mypy that we really mean to re-xport the type
    from .rust_backend import RouteMap as RouteMap
except ImportError:
    RouteMap = PythonRouteMap
