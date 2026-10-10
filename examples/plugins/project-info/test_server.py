"""Run directly with Python; no third-party dependencies or network access."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SERVER = Path(__file__).with_name("server.py")


def request(identifier, method, **params):
    return json.dumps({"jsonrpc": "2.0", "id": identifier, "method": method, "params": params}) + "\n"


class ProjectInfoServerTests(unittest.TestCase):
    def run_server(self, workspace, messages):
        completed = subprocess.run(
            [sys.executable, str(SERVER), str(workspace)],
            input=messages, capture_output=True, text=True, timeout=5, check=True,
        )
        self.assertEqual(completed.stderr, "")
        return [json.loads(line) for line in completed.stdout.splitlines()]

    def initialize(self):
        return request(1, "initialize", protocolVersion="2025-11-25")

    def test_real_protocol_handshake_catalog_and_project_info_without_file_contents(self):
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            (workspace / "src").mkdir()
            (workspace / "README.md").write_text("private file content")
            (workspace / ".env").write_text("SECRET=must-not-appear")
            messages = self.initialize()
            messages += '{"jsonrpc":"2.0","method":"notifications/initialized"}\n'
            messages += request(2, "tools/list")
            messages += request(3, "tools/call", name="project_info", arguments={})
            result = self.run_server(workspace, messages)
            self.assertEqual([message["id"] for message in result], [1, 2, 3])
            self.assertEqual(result[0]["result"]["protocolVersion"], "2025-11-25")
            self.assertEqual(result[1]["result"]["tools"][0]["name"], "project_info")
            text = result[2]["result"]["content"][0]["text"]
            info = json.loads(text)
            self.assertEqual(info["counts"], {"files": 2, "directories": 1, "symlinks": 0, "other": 0})
            self.assertEqual([entry["name"] for entry in info["entries"]], [".env", "README.md", "src"])
            self.assertFalse(info["content_read"])
            self.assertNotIn("must-not-appear", text)
            self.assertNotIn("private file content", text)

    def test_invalid_arguments_and_unknown_tools_are_not_treated_as_project_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_server(directory, self.initialize()
                + request(2, "tools/call", name="project_info", arguments={"path": "/"})
                + request(3, "tools/call", name="other", arguments={})
                + request(4, "missing/method"))
            self.assertEqual([item["error"]["code"] for item in result[1:]], [-32602, -32602, -32601])

    def test_names_are_bounded_and_symlinks_are_not_followed(self):
        with tempfile.TemporaryDirectory() as directory, tempfile.TemporaryDirectory() as outside:
            workspace = Path(directory)
            for index in range(105):
                (workspace / f"entry-{index:03}").touch()
            (Path(outside) / "outside-secret").write_text("secret")
            linked = False
            try:
                (workspace / "linked").symlink_to(outside, target_is_directory=True)
                linked = True
            except (OSError, NotImplementedError):
                pass
            result = self.run_server(workspace, self.initialize() + request(2, "tools/call", name="project_info"))
            info = json.loads(result[1]["result"]["content"][0]["text"])
            self.assertEqual(len(info["entries"]), 100)
            self.assertTrue(info["names_truncated"])
            self.assertTrue(info["counts_complete"])
            self.assertEqual(info["counts"]["files"], 105)
            self.assertEqual(info["counts"]["symlinks"], int(linked))
            self.assertNotIn("outside-secret", str(info))

    def test_invalid_and_oversized_messages_do_not_break_following_requests(self):
        with tempfile.TemporaryDirectory() as directory:
            messages = 'invalid\n' + 'x' * 70000 + '\n' + self.initialize() + request(2, "ping")
            result = self.run_server(directory, messages)
            self.assertEqual([item["error"]["code"] for item in result[:2]], [-32700, -32700])
            self.assertEqual(result[-1], {"jsonrpc": "2.0", "id": 2, "result": {}})

    def test_malformed_initialize_and_tool_before_initialize_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_server(directory, request(1, "initialize", protocolVersion=[])
                + request(2, "tools/list") + self.initialize() + request(3, "tools/list"))
            self.assertEqual(result[0]["error"]["code"], -32602)
            self.assertEqual(result[1]["error"]["code"], -32000)
            self.assertEqual(result[-1]["result"]["tools"][0]["name"], "project_info")


if __name__ == "__main__":
    unittest.main()
