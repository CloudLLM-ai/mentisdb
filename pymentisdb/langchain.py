"""
LangChain integration for MentisDB.

:class:`MentisDbMemory` is a LangChain ``BaseMemory`` backed by MentisDB. It
needs the ``langchain`` extra (``pip install pymentisdb[langchain]``). Without
it, importing this module still works so that ``import pymentisdb`` never
depends on LangChain; :class:`MentisDbMemory` is then a placeholder that raises
:class:`ImportError` when it is instantiated.

Supported: langchain-core before 1.0 (``langchain_core.memory``) and
langchain-core 1.0 or later with ``langchain-classic``, where the legacy memory
classes moved.
"""

try:
    from ._langchain_memory import MentisDbMemory
except ImportError as _import_error:
    _LANGCHAIN_IMPORT_ERROR = _import_error

    class MentisDbMemory:  # type: ignore[no-redef]
        """
        Placeholder for the LangChain memory class when LangChain is missing.

        Raises:
            ImportError: always, naming the extra to install and the original
                import failure.
        """

        def __init__(self, *args, **kwargs):
            raise ImportError(
                "MentisDbMemory needs LangChain: pip install 'pymentisdb[langchain]' "
                f"(import failed: {_LANGCHAIN_IMPORT_ERROR})"
            ) from _LANGCHAIN_IMPORT_ERROR


__all__ = ["MentisDbMemory"]
