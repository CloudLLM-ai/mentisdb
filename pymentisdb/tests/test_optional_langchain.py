"""The package imports without LangChain; MentisDbMemory fails only when used.

Each test imports the real ``pymentisdb`` package in a fresh interpreter, because
the other test modules register a stand-in ``pymentisdb`` package in
``sys.modules`` and would hide an import-time failure of ``__init__.py``.
"""

import importlib.util
import pathlib
import subprocess
import sys
import textwrap
import unittest

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]

# Makes every LangChain module (and pydantic, which the extra brings) look
# uninstalled, whatever the test environment has.
BLOCK_LANGCHAIN = textwrap.dedent(
    """
    import importlib.abc
    import sys

    BLOCKED = ("langchain", "langchain_core", "langchain_classic", "pydantic")

    class Blocker(importlib.abc.MetaPathFinder):
        def find_spec(self, name, path=None, target=None):
            if name.split(".")[0] in BLOCKED:
                raise ModuleNotFoundError(f"No module named {name!r}", name=name)
            return None

    for name in list(sys.modules):
        if name.split(".")[0] in BLOCKED:
            del sys.modules[name]
    sys.meta_path.insert(0, Blocker())
    """
)

# langchain-core 1.x: the package exists but `langchain_core.memory` is gone,
# and langchain-classic is not installed (the reported failure).
CURRENT_LANGCHAIN_CORE_WITHOUT_CLASSIC = textwrap.dedent(
    """
    import importlib.abc
    import sys
    import types

    core = types.ModuleType("langchain_core")
    core.__path__ = []
    messages = types.ModuleType("langchain_core.messages")
    for cls in ("HumanMessage", "AIMessage", "BaseMessage"):
        setattr(messages, cls, type(cls, (), {}))
    sys.modules["langchain_core"] = core
    sys.modules["langchain_core.messages"] = messages

    class Blocker(importlib.abc.MetaPathFinder):
        def find_spec(self, name, path=None, target=None):
            if name == "langchain_core.memory" or name.split(".")[0] == "langchain_classic":
                raise ModuleNotFoundError(f"No module named {name!r}", name=name)
            return None

    sys.meta_path.insert(0, Blocker())
    """
)


def run_python(prelude: str, body: str) -> subprocess.CompletedProcess:
    """Run ``prelude`` then ``body`` in a fresh interpreter at the repo root."""
    return subprocess.run(
        [sys.executable, "-c", prelude + "\n" + textwrap.dedent(body)],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        timeout=120,
    )


class OptionalLangChainTests(unittest.TestCase):
    def assert_ok(self, result: subprocess.CompletedProcess) -> None:
        self.assertEqual(result.returncode, 0, msg=result.stderr)

    def test_package_imports_without_langchain(self) -> None:
        result = run_python(
            BLOCK_LANGCHAIN,
            """
            import pymentisdb
            from pymentisdb import MentisDbClient, ReadSigner, Thought
            assert pymentisdb.__version__ == "0.10.1", pymentisdb.__version__
            assert "langchain_core" not in sys.modules
            print("ok")
            """,
        )
        self.assert_ok(result)
        self.assertIn("ok", result.stdout)

    def test_star_import_works_without_langchain(self) -> None:
        result = run_python(
            BLOCK_LANGCHAIN,
            """
            from pymentisdb import *
            assert MentisDbClient is not None
            assert MentisDbMemory is not None
            """,
        )
        self.assert_ok(result)

    def test_memory_class_raises_clear_import_error_when_used(self) -> None:
        result = run_python(
            BLOCK_LANGCHAIN,
            """
            from pymentisdb import MentisDbMemory
            try:
                MentisDbMemory(chain_key="c")
            except ImportError as error:
                print(error)
            else:
                raise SystemExit("MentisDbMemory() did not raise")
            """,
        )
        self.assert_ok(result)
        self.assertIn("pip install 'pymentisdb[langchain]'", result.stdout)

    def test_package_imports_with_langchain_core_1_without_memory_module(self) -> None:
        result = run_python(
            CURRENT_LANGCHAIN_CORE_WITHOUT_CLASSIC,
            """
            import pymentisdb
            from pymentisdb import MentisDbClient, MentisDbMemory
            try:
                MentisDbMemory()
            except ImportError as error:
                assert "langchain" in str(error), error
            else:
                raise SystemExit("MentisDbMemory() did not raise")
            """,
        )
        self.assert_ok(result)

    def test_unknown_attribute_is_still_an_attribute_error(self) -> None:
        result = run_python(
            BLOCK_LANGCHAIN,
            """
            import pymentisdb
            try:
                pymentisdb.NoSuchThing
            except AttributeError:
                pass
            else:
                raise SystemExit("no AttributeError")
            """,
        )
        self.assert_ok(result)


@unittest.skipUnless(
    importlib.util.find_spec("langchain_core") is not None
    and (
        importlib.util.find_spec("langchain_classic") is not None
        or importlib.util.find_spec("langchain_core.memory") is not None
    ),
    "LangChain is not installed",
)
class InstalledLangChainTests(unittest.TestCase):
    def test_memory_class_is_a_langchain_memory(self) -> None:
        result = run_python(
            "import sys",
            """
            from unittest.mock import MagicMock
            from pymentisdb import MentisDbMemory
            memory = MentisDbMemory(chain_key="c", agent_id="assistant")
            assert memory.memory_variables == ["chat_history"]
            memory._client = MagicMock()
            memory.save_context({"input": "hi"}, {"output": "hello"})
            calls = memory._client.append_thought.call_args_list
            assert [c.kwargs["content"] for c in calls] == ["hi", "hello"], calls
            """,
        )
        self.assertEqual(result.returncode, 0, msg=result.stderr)


if __name__ == "__main__":
    unittest.main()
