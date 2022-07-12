from typing import Any, Dict

from setuptools_rust import Binding, RustExtension


def build(setup_kwargs: Dict[str, Any]) -> None:
    """
    Add rust_extensions to the setup dict
    """
    setup_kwargs["rust_extensions"] = [
        RustExtension("starlite.rust_backend", path="rust_backend/Cargo.toml", binding=Binding.PyO3, optional=True)
    ]
    setup_kwargs["zip_safe"] = False


if __name__ == "__main__":
    build({})
