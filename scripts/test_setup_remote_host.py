import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("setup_remote", Path(__file__).with_name("setup-remote-host.py"))
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class RemoteSetupTests(unittest.TestCase):
    def test_import_preserves_model_and_does_not_read_provider_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "native-provider.json").write_text(json.dumps({"provider_id": "codex", "model_id": "exact-existing-model"}))
            project = root / "project"
            project.mkdir()
            config = setup.configuration(root, [project, project], allow_commands=True)
            self.assertEqual(config["provider"]["model"], "exact-existing-model")
            self.assertIsNone(config["provider"]["api_key_env"])
            self.assertEqual(config["provider"]["desktop_data_root"], str(root))
            self.assertEqual(len(config["projects"]), 1)
            self.assertEqual(config["bind"], "127.0.0.1:8787")

    def test_read_only_overrides_command_opt_in(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "native-provider.json").write_text(json.dumps({"provider_id": "codex", "model_id": "chosen-model"}))
            config = setup.configuration(root, [root], allow_commands=True, read_only=True)
            self.assertFalse(config["allow_write"])
            self.assertFalse(config["allow_commands"])

    def test_private_configuration_never_overwrites_a_previous_setup(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            setup.write_new_private(path, {"version": 1})
            with self.assertRaises(FileExistsError):
                setup.write_new_private(path, {"version": 2})
            self.assertEqual(json.loads(path.read_text()), {"version": 1})
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_service_arguments_cannot_inject_units_or_specifiers(self):
        self.assertEqual(setup.systemd_quote("/a%u/b"), '"/a%%u/b"')
        with self.assertRaises(ValueError):
            setup.systemd_quote("/tmp/app\nExecStart=other")
        unit = setup.service_unit(Path("/tmp/my app"), Path("/tmp/my state"))
        self.assertIn('ExecStart="/tmp/my app" --state "/tmp/my state" serve', unit)
        self.assertNotIn("allow-remote-bind", unit)

    def test_desktop_state_aliases_cannot_merge_session_stores(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(ValueError):
                setup.separate_state(root / "remote/..", root)
            alias = root / "alias"
            alias.symlink_to(root, target_is_directory=True)
            with self.assertRaises(ValueError):
                setup.separate_state(alias, root)

    def test_different_service_is_rejected_before_replacing_its_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            units = root / ".config/systemd/user"
            units.mkdir(parents=True)
            (units / "openade-remote.service").write_text(setup.service_unit(root / "other/bin", root / "other/state"))
            state = root / "remote"
            (state / "bin").mkdir(parents=True)
            binary = state / "bin/openide-remote"
            binary.write_text("original binary")
            with mock.patch.object(Path, "home", return_value=root), mock.patch.object(setup.shutil, "which", return_value="/bin/systemctl"):
                with self.assertRaises(ValueError):
                    setup.install_service(root / "missing-new-binary", state)
            self.assertEqual(binary.read_text(), "original binary")


if __name__ == "__main__":
    unittest.main()
