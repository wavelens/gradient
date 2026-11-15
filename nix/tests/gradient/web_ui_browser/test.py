# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# Playwright drives the browser tests for the web UI. They run in Chromium inside the VM. The tests target the packaged frontend behind nginx. The script can display Playwright output. Each failed test and its error appear in that output.

VM_PLAYWRIGHT_SPEC_RUN_DIRECTORY = "/tmp/playwright-tests"

start_all()
machine.wait_for_unit("gradient-server.service")
machine.wait_for_open_port(3000)
machine.wait_for_unit("nginx.service")
machine.wait_until_succeeds("curl -sf http://gradient.local/account/login")

machine.succeed(
    f"cp -r {PLAYWRIGHT_SPEC_DIRECTORY_STORE_PATH} {VM_PLAYWRIGHT_SPEC_RUN_DIRECTORY}",
    f"chmod -R u+w {VM_PLAYWRIGHT_SPEC_RUN_DIRECTORY}",
    f"ln -s {PLAYWRIGHT_TEST_NODE_MODULES_STORE_PATH} {VM_PLAYWRIGHT_SPEC_RUN_DIRECTORY}/node_modules",
)

status, output = machine.execute(
    f"cd {VM_PLAYWRIGHT_SPEC_RUN_DIRECTORY} && GRADIENT_WEB_UI_BASE_URL=http://localhost"
    f" PLAYWRIGHT_BROWSERS_PATH={PLAYWRIGHT_CHROMIUM_BROWSERS_STORE_PATH}"
    " PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true"
    " playwright test 2>&1",
    timeout=None,
)
print(output)

if status != 0:
    raise Exception(f"Playwright exited with status {status}")
