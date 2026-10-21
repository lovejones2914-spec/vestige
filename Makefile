# Vestige — narrow, runnable checks.
#
# These targets mirror the commands in CLAUDE.md so a contributor (or CI) can run
# the same narrow check with one word instead of retyping the flags:
#
#   make rust-check           cargo check -p vestige-mcp
#   make rust-clippy          clippy over the MCP crate, its tests and examples
#   make rust-test            unit tests for the OrcaRouter env file, catalog and routes
#   make rust-live            the live OrcaRouter test (needs ORCAROUTER_API_KEY)
#   make dashboard-test       the dashboard's vitest suite
#   make dashboard-check      svelte-check over the dashboard
#   make orcarouter-evidence  headless Chromium screenshots into orca-evidence/
#
# The Makefile does not pin a toolchain: put `cargo` on PATH, or pass CARGO=/path/to/cargo,
# so the same targets work with a rustup shim, a distro package or a CI image.
CARGO ?= cargo
NPM ?= npm
PYTHON ?= python3
DASHBOARD = apps/dashboard

.PHONY: rust-check rust-clippy rust-test rust-live dashboard-test dashboard-check \
	orcarouter-evidence

rust-check:
	$(CARGO) check --package=vestige-mcp

# `--all-targets` pulls in the unit tests and the live integration test, so a lint
# regression in a test file fails here rather than at release time.
rust-clippy:
	$(CARGO) clippy --package=vestige-mcp --all-targets -- -D warnings

# The provider surface this integration adds: the env file writer, the catalog
# filter and the four dashboard routes that sit in front of them.
rust-test:
	$(CARGO) test --package=vestige-mcp --lib -- sanhedrin_env:: dashboard::orcarouter:: dashboard::http_tests::

# The one target that reaches api.orcarouter.ai. It skips itself when the key is
# absent, so a contributor without a key can still run everything else.
rust-live:
	$(CARGO) test --package=vestige-mcp --test=orcarouter_live -- --nocapture

dashboard-test:
	$(NPM) --prefix $(DASHBOARD) run test

dashboard-check:
	$(NPM) --prefix $(DASHBOARD) run check

# Build the binary that embeds apps/dashboard/build, serve it on a disposable data
# dir, drive the real System page in headless Chromium and write the screenshots
# plus their manifest to orca-evidence/ (git-ignored, never committed — evidence is
# generated when it is checked, not shipped in the patch).
orcarouter-evidence:
	$(CARGO) build --package=vestige-mcp --bin vestige-mcp
	$(PYTHON) scripts/orcarouter-evidence.py --binary target/debug/vestige-mcp --out orca-evidence
