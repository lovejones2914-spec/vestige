#!/usr/bin/env python3
"""Generate the OrcaRouter UI evidence from the real dashboard.

This is the harness behind `make orcarouter-evidence`. It never talks to a mock:
it starts the binary built from this checkout (whose dashboard is the committed
`apps/dashboard/build`), points it at a disposable data directory, and drives the
System page's verifier lens in headless Chromium. The catalog it asserts against
is fetched from the gateway itself, so the dropdown's item count cannot drift from
the models the gateway actually serves this key.

The key is read from ORCAROUTER_API_KEY and is never printed, written into the
manifest, or typed into the page. The page only reports that a key is present;
the harness types a clearly fake placeholder to prove the field masks input.

Output (all under --out, which is git-ignored):

    manifest.json                 what was driven, and every measured assertion
    auth-methods.png              key field, key state and the enabled controls
    text-model-dropdown.png       the open catalog dropdown, one row per served model
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

CATALOG_URL = "https://api.orcarouter.ai/v1/models?capability=chat"
CHAT_ENDPOINTS = {"openai", "anthropic", "gemini", "openai-response"}
PLACEHOLDER = "sk-orca-placeholder-never-submitted"
VIEWPORT = {"width": 1440, "height": 900}


def key_from_environment():
    name = "ORCAROUTER_API_" + "KEY"
    value = os.environ.get(name, "").strip()
    if not value.startswith("sk-orca-"):
        raise SystemExit(
            "ORCAROUTER_API_KEY is not exported; the evidence must come from a live catalog"
        )
    return value


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def fetch_catalog(key):
    request = urllib.request.Request(
        CATALOG_URL, headers={"Authorization": "Bearer " + key}
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        if response.status != 200:
            raise SystemExit("catalog answered " + str(response.status))
        body = json.load(response)
    entries = body.get("data") or []
    if not entries:
        raise SystemExit("the catalog returned no models")
    return entries


def served_models(entries):
    """The same rule the provider applies: chat endpoint types, no exclusive
    non-chat endpoint, and text-only output when the catalog lists modalities."""
    models = []
    for entry in entries:
        endpoints = entry.get("supported_endpoint_types") or []
        if not any(e in CHAT_ENDPOINTS for e in endpoints):
            continue
        if any(e in ("image-generation", "embedding", "rerank", "video") for e in endpoints):
            continue
        outputs = (entry.get("architecture") or {}).get("output_modalities")
        if outputs is not None and any(o != "text" for o in outputs):
            continue
        models.append(
            {
                "id": entry.get("id", ""),
                "label": entry.get("name") or entry.get("id", ""),
            }
        )
    return models


def image_models(entries):
    return [
        entry.get("id", "")
        for entry in entries
        if "image" in ((entry.get("architecture") or {}).get("output_modalities") or [])
    ]


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def png_size(path):
    header = path.read_bytes()[:24]
    if header[:8] != b"\x89PNG\r\n\x1a\n" or header[12:16] != b"IHDR":
        raise SystemExit(str(path) + " is not a PNG")
    return struct.unpack(">II", header[16:24])


def start_backend(binary, home, port, env_path, key):
    env = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(("VESTIGE_", "ORCAROUTER_", "GITHUB_", "REDMINE_"))
    }
    env.update(
        HOME=str(home),
        VESTIGE_DASHBOARD_ENABLED="true",
        VESTIGE_DASHBOARD_PORT=str(port),
        VESTIGE_SANHEDRIN_ENV=str(env_path),
        VESTIGE_SANHEDRIN_API_KEY=key,
        RUST_LOG="warn",
    )
    log = open(Path(home) / "backend.log", "wb")
    process = subprocess.Popen(
        [str(binary), "--data-dir", str(Path(home) / "data"), "--http-port", str(free_port())],
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.DEVNULL,
        stderr=log,
        start_new_session=True,
    )
    base = "http://127.0.0.1:" + str(port)
    for _ in range(240):
        if process.poll() is not None:
            raise SystemExit("the dashboard backend exited; see " + str(Path(home) / "backend.log"))
        try:
            with urllib.request.urlopen(base + "/dashboard", timeout=2) as response:
                if response.status == 200:
                    return process, base
        except (urllib.error.URLError, OSError):
            time.sleep(0.25)
    stop_backend(process)
    raise SystemExit("the dashboard never answered on " + base)


def stop_backend(process):
    if process is None:
        return
    try:
        os.killpg(process.pid, 15)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, 9)
        except ProcessLookupError:
            pass


def rgba(value):
    match = re.match(r"rgba?\(([^)]+)\)", value or "")
    if not match:
        return None
    parts = [p.strip() for p in match.group(1).replace("/", ",").split(",")]
    try:
        channels = [float(p) for p in parts[:3]]
        alpha = float(parts[3]) if len(parts) > 3 else 1.0
    except ValueError:
        return None
    return channels, alpha


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/vestige-mcp")
    parser.add_argument("--out", default="orca-evidence")
    parser.add_argument("--chromium", default=os.environ.get("VESTIGE_CHROMIUM", "/usr/bin/chromium"))
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        raise SystemExit("missing binary: " + str(binary) + " (run `cargo build --package=vestige-mcp`)")
    key = key_from_environment()

    entries = fetch_catalog(key)
    models = served_models(entries)
    images = image_models(entries)
    if not models:
        raise SystemExit("no served model survived the provider's chat filter")
    expected_labels = sorted(model["label"] for model in models)

    out = Path(args.out).resolve()
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True, mode=0o700)

    home = Path(tempfile.mkdtemp(prefix="orca-evidence-"))
    process = None
    try:
        process, base = start_backend(
            binary, home, free_port(), home / "vestige-sanhedrin.env", key
        )
        with urllib.request.urlopen(base + "/api/orcarouter/models", timeout=60) as response:
            provider = json.load(response)
        provider_ids = sorted(entry["id"] for entry in provider["models"])
        if provider_ids != sorted(model["id"] for model in models):
            raise SystemExit(
                "the provider's catalog differs from the gateway's: "
                + str(len(provider_ids))
                + " vs "
                + str(len(models))
            )

        from playwright.sync_api import sync_playwright

        console_errors = []
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(
                executable_path=args.chromium,
                args=[
                    "--no-sandbox",
                    "--disable-dev-shm-usage",
                    "--use-gl=angle",
                    "--ignore-gpu-blocklist",
                ],
            )
            page = browser.new_page(viewport=VIEWPORT)
            page.on(
                "console",
                lambda message: console_errors.append(message.type + ": " + message.text)
                if message.type in ("error", "warning")
                else None,
            )
            page.goto(base + "/dashboard/system", wait_until="load", timeout=60000)
            page.get_by_role("button", name="verifier", exact=True).click()

            key_field = page.locator('input[name="orcarouter-key"]')
            key_field.wait_for(state="visible", timeout=30000)
            trigger = page.locator(".orc-dd .dd-trigger")
            trigger.wait_for(state="visible", timeout=60000)
            page.wait_for_timeout(400)

            state = page.locator("[data-orcarouter-key-state]").get_attribute(
                "data-orcarouter-key-state"
            )
            key_text = page.locator("[data-orcarouter-key-state]").inner_text()
            if state != "present":
                raise SystemExit("the page does not report a present key: " + key_text)
            if "from the environment" not in key_text:
                raise SystemExit("the key did not come from the environment: " + key_text)

            # The field is a password input; the harness types a placeholder (never
            # the real key) to prove the value is masked, and the buttons enable.
            input_type = key_field.get_attribute("type")
            key_field.fill(PLACEHOLDER)
            store = page.get_by_role("button", name="Store key")
            refresh = page.get_by_role("button", name="Refresh catalog")
            store_enabled = store.is_enabled()
            refresh_enabled = refresh.is_enabled()
            store.wait_for(state="visible")
            body_text = page.locator("body").inner_text()
            source = page.content()
            if PLACEHOLDER in body_text or PLACEHOLDER in source:
                raise SystemExit("the typed placeholder leaked into the rendered page")
            if input_type != "password":
                raise SystemExit("the key field is not masked (type=" + str(input_type) + ")")
            api_key_visible = key_field.is_visible()
            secret_masked = input_type == "password" and PLACEHOLDER not in source
            controls_enabled = store_enabled and refresh_enabled
            if not (api_key_visible and secret_masked and controls_enabled):
                raise SystemExit("the auth controls are not usable")

            page.screenshot(path=str(out / "auth-methods.png"))

            trigger.click()
            menu = page.locator(".orc-dd .dd-menu")
            menu.wait_for(state="visible", timeout=30000)
            page.wait_for_timeout(500)
            options = menu.locator('[role="option"]')
            item_count = options.count()
            # A reasoning model carries a badge inside its row; compare on the
            # option's own label span so the assertion is about catalog identity.
            labels = sorted(
                text.strip()
                for text in menu.locator('[role="option"] .dd-opt-label').all_inner_texts()
            )
            if labels != expected_labels:
                raise SystemExit(
                    "dropdown labels do not match the served catalog: "
                    + str(len(labels))
                    + " vs "
                    + str(len(expected_labels))
                )

            style = menu.evaluate(
                "el => { const s = getComputedStyle(el);"
                " return {background: s.backgroundColor, border: s.borderTopWidth,"
                " style: s.borderTopStyle, color: s.borderTopColor,"
                " image: s.backgroundImage}; }"
            )
            background = rgba(style["background"])
            border = rgba(style["color"])
            # The panel must paint its own surface, not let the page show through:
            # an alpha of at least 0.75 over the lane's backdrop reads as opaque,
            # and a painted gradient is equally acceptable.
            opaque_background = bool(
                (background and background[1] >= 0.75)
                or (style["image"] and style["image"] != "none")
            )
            visible_border = bool(
                border
                and border[1] > 0
                and float(style["border"].replace("px", "")) >= 1
                and style["style"] not in ("none", "hidden", "")
            )
            box = menu.bounding_box()
            trigger_box = trigger.bounding_box()
            delta = round(
                (trigger_box["x"] + trigger_box["width"]) - (box["x"] + box["width"]), 2
            )
            if not (opaque_background and visible_border):
                raise SystemExit("the dropdown panel has no visible container: " + json.dumps(style))
            if abs(delta) > 2:
                raise SystemExit("the dropdown is not aligned to its trigger: " + str(delta))

            page.screenshot(path=str(out / "text-model-dropdown.png"))
            browser.close()
    finally:
        stop_backend(process)
        shutil.rmtree(home, ignore_errors=True)

    artifacts = [
        {
            "kind": "auth-methods",
            "path": "auth-methods.png",
            "sha256": sha256(out / "auth-methods.png"),
            "ui": {
                "api_key_visible": True,
                "secret_masked": True,
                "controls_enabled": True,
                "key_source": "environment",
                "store_button_enabled": True,
                "refresh_button_enabled": True,
                "key_echoed": False,
            },
        },
        {
            "kind": "text-model-dropdown",
            "path": "text-model-dropdown.png",
            "sha256": sha256(out / "text-model-dropdown.png"),
            "ui": {
                "dropdown_open": True,
                "item_count": item_count,
                "opaque_background": True,
                "visible_border": True,
                "trigger_panel_right_delta": delta,
                "panel_background": style["background"],
                "panel_border": style["border"] + " " + style["style"],
            },
        },
    ]
    for artifact in artifacts:
        width, height = png_size(out / artifact["path"])
        artifact["width"] = width
        artifact["height"] = height
        artifact["bytes"] = (out / artifact["path"]).stat().st_size

    manifest = {
        "repository": "samvallad33/vestige",
        "surface": "/dashboard/system -> verifier lens (Sanhedrin Stop hook)",
        "url": base + "/dashboard/system",
        "captured_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "viewport": VIEWPORT,
        "binary": {"path": str(binary), "sha256": sha256(binary)},
        "automation": {
            "framework": "playwright",
            "passed": True,
            "catalog_source": CATALOG_URL,
            "catalog_model_count": len(models),
            "image_model_count": len(images),
            "catalog_entries": len(entries),
            "provider_route_total": provider["total"],
            "catalog_echoed_key": False,
        },
        "artifacts": artifacts,
        "console_errors": console_errors,
    }
    (out / "manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(
        "evidence written to {out}: {models} served models, {images} image models".format(
            out=out, models=len(models), images=len(images)
        )
    )
    print("catalog:", CATALOG_URL)
    for artifact in artifacts:
        print(" ", artifact["kind"], artifact["width"], "x", artifact["height"], artifact["bytes"], "bytes")


if __name__ == "__main__":
    main()
    sys.exit(0)
