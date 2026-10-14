import copy
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "manifests", Path(__file__).with_name("verify_cargo_workspace_manifests.py")
)
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class ProviderFeaturePolicyTests(unittest.TestCase):
    def errors(self, path, manifest):
        with patch.object(policy, "load_manifest", return_value=manifest):
            return policy.manifest_errors(
                path, {"codex-provider-registry"}, set(), set(), set()
            )

    def test_existing_provider_matrix_is_exact(self):
        path = policy.ROOT / "codex-rs/providers/provider-registry/Cargo.toml"
        original = policy.load_manifest(path)
        self.assertEqual(self.errors(path, original), [])
        expanded = copy.deepcopy(original)
        expanded["features"]["unreviewed-provider"] = []
        self.assertTrue(
            any("limit `[features]`" in e for e in self.errors(path, expanded))
        )

    def test_unlisted_optional_dependency_still_fails(self):
        path = policy.ROOT / "codex-rs/providers/provider-registry/Cargo.toml"
        manifest = copy.deepcopy(policy.load_manifest(path))
        manifest["dependencies"]["unreviewed"] = {"version": "1", "optional": True}
        self.assertTrue(
            any("optional = true" in e for e in self.errors(path, manifest))
        )

    def test_unlisted_default_feature_toggle_still_fails(self):
        path = policy.ROOT / "codex-rs/model-provider/Cargo.toml"
        manifest = copy.deepcopy(policy.load_manifest(path))
        manifest["dependencies"]["codex-provider-registry"] = {
            "workspace": True,
            "default-features": False,
        }
        self.assertTrue(
            any("default-features = false" in e for e in self.errors(path, manifest))
        )


if __name__ == "__main__":
    unittest.main()
