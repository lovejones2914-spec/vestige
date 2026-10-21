import json
import re
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
PRESETS = REPO_ROOT / "hooks" / "sanhedrin-presets.json"
PREREQS = REPO_ROOT / "scripts" / "check-sandwich-prereqs.sh"
INSTALLER = REPO_ROOT / "scripts" / "install-sandwich.sh"


class OrcaRouterPresetTests(unittest.TestCase):
    """The OrcaRouter preset is a named provider, not a generic passthrough."""

    def setUp(self):
        self.data = json.loads(PRESETS.read_text(encoding="utf-8"))
        self.presets = self.data["presets"]

    def test_named_provider_is_present_with_its_own_endpoint(self):
        self.assertIn("orcarouter", self.presets)
        preset = self.presets["orcarouter"]
        self.assertEqual(preset["tier"], "hosted")
        self.assertEqual(
            preset["env"]["VESTIGE_SANHEDRIN_ENDPOINT"],
            "https://api.orcarouter.ai/v1/chat/completions",
        )
        self.assertEqual(preset["env"]["VESTIGE_SANHEDRIN_BACKEND"], "openai")

    def test_preset_names_the_api_key_env_and_prefix(self):
        preset = self.presets["orcarouter"]
        self.assertIn("VESTIGE_SANHEDRIN_API_KEY", preset["requires"])
        self.assertIn("sk-orca-", preset["requires"])
        self.assertNotIn("VESTIGE_SANHEDRIN_API_KEY", preset["env"])
        for value in preset["env"].values():
            self.assertNotRegex(value, r"sk-orca-[A-Za-z0-9_-]{8,}")

    def test_preset_does_not_hijack_the_default_or_other_providers(self):
        self.assertIsNone(self.data["defaultPreset"])
        self.assertGreater(len(self.presets), 8)
        self.assertIn("custom-openai-compatible", self.presets)
        self.assertIn("hosted-openai-compatible", self.presets)
        self.assertEqual(
            self.presets["hosted-openai-compatible"]["env"][
                "VESTIGE_SANHEDRIN_ENDPOINT"
            ],
            "https://api.openai.com/v1/chat/completions",
        )

    def test_every_preset_keeps_the_same_shape(self):
        for name, preset in self.presets.items():
            with self.subTest(preset=name):
                self.assertIn("env", preset)
                env = preset["env"]
                self.assertIn("VESTIGE_SANHEDRIN_ENDPOINT", env)
                self.assertIn("VESTIGE_SANHEDRIN_MODEL", env)
                self.assertIn("VESTIGE_SANHEDRIN_BACKEND", env)


class PrereqAllowListTests(unittest.TestCase):
    """Both env readers must forward the hosted-provider key and loose-ledger flag."""

    def test_check_prereqs_forwards_hosted_provider_key(self):
        text = PREREQS.read_text(encoding="utf-8")
        shell_case = text.split("done < <(python3", 1)[0]
        self.assertIn("VESTIGE_SANHEDRIN_API_KEY", shell_case)
        allowed = re.search(r"allowed = \{(.*?)\}", text, re.S)
        self.assertIsNotNone(allowed)
        self.assertIn('"VESTIGE_SANHEDRIN_API_KEY"', allowed.group(1))
        self.assertIn('"VESTIGE_SANHEDRIN_ALLOW_LOOSE_LEDGER"', allowed.group(1))

    def test_models_probe_sends_bearer_when_key_present(self):
        text = PREREQS.read_text(encoding="utf-8")
        self.assertIn('SANHEDRIN_API_KEY="${VESTIGE_SANHEDRIN_API_KEY:-}"', text)
        self.assertIn("-H \"Authorization: Bearer ${SANHEDRIN_API_KEY}\"", text)
        # The probe itself must carry the header array, not a bare URL.
        probe = [line for line in text.splitlines() if "curl -fsS -m 2" in line and "SANHEDRIN_MODELS_URL" in line]
        self.assertEqual(len(probe), 1, probe)
        self.assertIn("SANHEDRIN_AUTH_ARGS", probe[0])
        self.assertIn('"$SANHEDRIN_MODELS_URL"', probe[0])

    def test_installer_reads_the_api_key_back_from_existing_env(self):
        text = INSTALLER.read_text(encoding="utf-8")
        self.assertIn("VESTIGE_SANHEDRIN_API_KEY", text)


if __name__ == "__main__":
    unittest.main()
