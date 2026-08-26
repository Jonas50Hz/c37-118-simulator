import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { dirname, resolve as resolvePath } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

const consoleDirectory = resolvePath(dirname(fileURLToPath(import.meta.url)), "..");
const staticAssets = new Map([
  ["/", { fileName: "index.html", contentType: "text/html; charset=utf-8" }],
  ["/index.html", { fileName: "index.html", contentType: "text/html; charset=utf-8" }],
  ["/app.js", { fileName: "app.js", contentType: "text/javascript; charset=utf-8" }],
  ["/styles.css", { fileName: "styles.css", contentType: "text/css; charset=utf-8" }],
]);
const catalogContentSha256 = "a".repeat(64);
const processIdentity = "b".repeat(64);
const replacementProcessIdentity = "c".repeat(64);
const pdcStreamId = 1016;
const pdcConnectionId = 42;

function catalogResponse() {
  return {
    version: 1,
    content_sha256: catalogContentSha256,
    scenarios: [
      {
        index: 1,
        name: "Frequency excursion",
        kind: "frequency_excursion",
        target_compatibility: "endpoint",
        lifecycle: "transient",
        start_frame_offset: 0,
        duration_frames: 5,
        signal: {
          voltage_magnitude_delta: 0,
          frequency_deviation_hz: 0.1,
          rocof_hz_per_s: 0,
        },
      },
    ],
  };
}

function stateResponse(currentSampleIndex) {
  return {
    format: "console-v1",
    process_identity: processIdentity,
    controller_revision: currentSampleIndex,
    catalog_content_sha256: catalogContentSha256,
    ready: true,
    time_health: "verified",
    stats: {
      accepted_clients: 0,
      closed_clients: 0,
      rejected_clients: 0,
      malformed_commands: 0,
      unsupported_commands: 0,
      slow_clients: 0,
      sent_data_frames: 0,
      skipped_ticks: 0,
    },
    fleet: {
      pdc_name: "",
      pmu_name_prefix: "PMU-",
      wire_version: 2,
      reporting_rate_hz: 50,
      nominal_frequency_hz: 60,
      first_stream_id: 1001,
      first_pmu_id: 1001,
      first_port: 4712,
    },
    endpoints: [
      {
        stream_id: 1001,
        active_connections: 0,
        connections: [],
      },
    ],
    scenario_controller: {
      current_sample_index: currentSampleIndex,
      prepared: [],
      pending: [],
      active: [],
      prepared_count: 0,
      pending_count: 0,
      active_count: 0,
    },
    next_cursor: null,
  };
}

function pdcCatalogResponse() {
  return {
    version: 1,
    content_sha256: catalogContentSha256,
    scenarios: [
      {
        index: 2,
        name: "disconnect-pdc",
        kind: "disconnect_pdc",
        target_compatibility: "pdc_connection",
        lifecycle: "sustained",
        start_frame_offset: 0,
      },
    ],
  };
}

function pdcStateResponse(currentSampleIndex, options = {}) {
  const {
    processIdentity: responseProcessIdentity = processIdentity,
    streamId: activePdcStreamId = pdcStreamId,
    connectionId: activePdcConnectionId = pdcConnectionId,
  } = options;
  const endpoints = [];
  for (let endpointOffset = 0; endpointOffset < 16; endpointOffset += 1) {
    const streamId = 1001 + endpointOffset;
    const hasPdcConnection = streamId === activePdcStreamId;
    endpoints.push({
      stream_id: streamId,
      active_connections: hasPdcConnection ? 1 : 0,
      connections: hasPdcConnection
        ? [{ connection_id: activePdcConnectionId, streaming: true }]
        : [],
    });
  }

  return {
    format: "console-v1",
    process_identity: responseProcessIdentity,
    controller_revision: currentSampleIndex,
    catalog_content_sha256: catalogContentSha256,
    ready: true,
    time_health: "verified",
    stats: {
      accepted_clients: 1,
      closed_clients: 0,
      rejected_clients: 0,
      malformed_commands: 0,
      unsupported_commands: 0,
      slow_clients: 0,
      sent_data_frames: 0,
      skipped_ticks: 0,
    },
    fleet: {
      pdc_name: "PDC-A",
      pmu_name_prefix: "PMU-",
      wire_version: 2,
      reporting_rate_hz: 50,
      nominal_frequency_hz: 60,
      first_stream_id: 1001,
      first_pmu_id: 1001,
      first_port: 4712,
    },
    endpoints,
    scenario_controller: {
      current_sample_index: currentSampleIndex,
      prepared: [],
      pending: [],
      active: [],
      prepared_count: 0,
      pending_count: 0,
      active_count: 0,
    },
    next_cursor: null,
  };
}

function preparedPdcStateResponse(currentSampleIndex, options = {}) {
  const {
    streamId: activePdcStreamId = pdcStreamId,
    connectionId: activePdcConnectionId = pdcConnectionId,
  } = options;
  const response = pdcStateResponse(currentSampleIndex, options);
  response.scenario_controller = {
    ...response.scenario_controller,
    prepared: [
      {
        token: "1",
        confirm_expires_in_ms: 60_000,
        target: {
          pdc: {
            stream_id: activePdcStreamId,
            connection_id: activePdcConnectionId,
          },
        },
        action: {
          activate: {
            scenario_name: "disconnect-pdc",
          },
        },
        actor_label: "initial operator",
      },
    ],
    prepared_count: 1,
  };
  return response;
}

function startStaticAssetServer() {
  return new Promise((resolveServer, rejectServer) => {
    const server = createServer(async (request, response) => {
      const requestUrl = new URL(request.url ?? "/", "http://127.0.0.1");
      const asset = staticAssets.get(requestUrl.pathname);
      if (asset === undefined) {
        response.writeHead(404);
        response.end();
        return;
      }

      try {
        const body = await readFile(resolvePath(consoleDirectory, asset.fileName));
        response.writeHead(200, { "Content-Type": asset.contentType });
        response.end(body);
      } catch (error) {
        response.writeHead(500, { "Content-Type": "text/plain; charset=utf-8" });
        response.end(String(error));
      }
    });
    server.once("error", rejectServer);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", rejectServer);
      const address = server.address();
      if (address === null || typeof address === "string") {
        rejectServer(new Error("The static asset server did not receive a TCP port."));
        return;
      }
      resolveServer({ server, origin: `http://127.0.0.1:${address.port}` });
    });
  });
}

function stopStaticAssetServer(server) {
  return new Promise((resolveServer, rejectServer) => {
    server.close((error) => {
      if (error === undefined) {
        resolveServer();
        return;
      }
      rejectServer(error);
    });
  });
}

let staticServer;
let staticOrigin;

test.beforeAll(async () => {
  const startedServer = await startStaticAssetServer();
  staticServer = startedServer.server;
  staticOrigin = startedServer.origin;
});

test.afterAll(async () => {
  await stopStaticAssetServer(staticServer);
});

test("keeps an open Scenario selector open through a scheduled successful refresh", async ({ page }) => {
  let stateResponseCount = 0;
  let notifySecondStateRequest;
  const secondStateRequest = new Promise((resolveSecondStateRequest) => {
    notifySecondStateRequest = resolveSecondStateRequest;
  });
  let releaseSecondStateResponse;
  const secondStateResponseReleased = new Promise((resolveSecondStateResponse) => {
    releaseSecondStateResponse = resolveSecondStateResponse;
  });

  await page.route(/\/api\/v1\/catalog$/, async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(catalogResponse()),
    });
  });
  await page.route(/\/api\/v1\/state\?format=console-v1$/, async (route) => {
    stateResponseCount += 1;
    if (stateResponseCount === 2) {
      notifySecondStateRequest();
      await secondStateResponseReleased;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(stateResponse(stateResponseCount)),
    });
  });

  await page.goto(`${staticOrigin}/`);
  const currentSampleValue = page
    .locator(".catalog-summary__item")
    .filter({ hasText: "Current Sample" })
    .locator("dd");
  await expect(currentSampleValue).toHaveText("1");

  const scenarioMenu = page.locator(".scenario-run-menu").first();
  await expect(scenarioMenu).toBeEnabled();
  expect(await page.evaluate(() => CSS.supports("selector(select:open)"))).toBe(true);
  await scenarioMenu.focus();
  await page.keyboard.press("Alt+ArrowDown");
  await expect.poll(
    () => scenarioMenu.evaluate((element) => element.matches(":open")),
    { timeout: 1_000 },
  ).toBe(true);

  await secondStateRequest;
  releaseSecondStateResponse();
  await expect(currentSampleValue).toHaveText("2");
  expect(stateResponseCount).toBe(2);
  await expect.poll(
    () => scenarioMenu.evaluate((element) => element.matches(":open")),
    { timeout: 1_000 },
  ).toBe(true);
});

test("does not submit an expired current confirmation dialog", async ({ page }) => {
  let stateResponseCount = 0;
  let notifyRefreshedState;
  const refreshedState = new Promise((resolveRefreshedState) => {
    notifyRefreshedState = resolveRefreshedState;
  });
  const postedEndpoints = [];

  await page.clock.install({ time: new Date("2026-08-26T00:00:00.000Z") });
  await page.route(/\/api\/v1\/catalog$/, async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(pdcCatalogResponse()),
    });
  });
  await page.route(/\/api\/v1\/state\?format=console-v1$/, async (route) => {
    stateResponseCount += 1;
    if (stateResponseCount === 2) {
      notifyRefreshedState();
    }
    const response = preparedPdcStateResponse(stateResponseCount);
    response.scenario_controller.prepared[0].confirm_expires_in_ms = 1_000;
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(response),
    });
  });
  await page.route(/\/api\/v1\/scenarios\/confirm$/, async (route) => {
    postedEndpoints.push(new URL(route.request().url()).pathname);
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: "{}",
    });
  });

  await page.goto(`${staticOrigin}/`);
  const currentSampleValue = page
    .locator(".catalog-summary__item")
    .filter({ hasText: "Current Sample" })
    .locator("dd");
  await expect(currentSampleValue).toHaveText("1");

  const pdcDetails = page.locator(`details[data-pdc-details-key="stream:${pdcStreamId}"]`);
  await pdcDetails.locator("summary").click();
  await pdcDetails.getByRole("button", { name: /^Confirm prepared/ }).click();

  const dialog = page.getByRole("dialog");
  const dialogForm = dialog.locator("form");
  await expect(dialog).toBeVisible();
  await dialogForm.locator("input").fill("expiry operator");

  await page.clock.fastForward(1_001);
  await expect(dialog.getByRole("button", { name: "Confirm" })).toBeDisabled();
  await page.evaluate(() => {
    const form = document.querySelector("dialog form");
    if (!(form instanceof HTMLFormElement)) {
      throw new Error("The current scenario dialog form was not available.");
    }
    form.dispatchEvent(new SubmitEvent("submit", { cancelable: true }));
  });

  await refreshedState;
  await expect(currentSampleValue).toHaveText("2");
  expect(stateResponseCount).toBe(2);
  expect(postedEndpoints).toEqual([]);
  await expect(dialog).toBeHidden();
  await expect(page.locator("#operation-status")).toHaveText(
    "The prepared action has expired. Refreshing the console state.",
  );
});

test("ignores a detached same-process Scenario dialog form after a replacement dialog opens", async ({ page }) => {
  const postedEndpoints = [];

  await page.route(/\/api\/v1\/catalog$/, async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(pdcCatalogResponse()),
    });
  });
  await page.route(/\/api\/v1\/state\?format=console-v1$/, async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(preparedPdcStateResponse(1)),
    });
  });
  await page.route(/\/api\/v1\/scenarios\/confirm$/, async (route) => {
    postedEndpoints.push(new URL(route.request().url()).pathname);
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: "{}",
    });
  });

  await page.goto(`${staticOrigin}/`);
  const pdcDetails = page.locator(`details[data-pdc-details-key="stream:${pdcStreamId}"]`);
  const preparedAction = pdcDetails.getByRole("button", { name: /^Confirm prepared/ });
  await pdcDetails.locator("summary").click();
  await preparedAction.click();

  const dialog = page.getByRole("dialog");
  const firstDialogForm = dialog.locator("form");
  await expect(dialog).toBeVisible();
  await firstDialogForm.locator("input").fill("stale operator");
  await page.evaluate(() => {
    const form = document.querySelector("dialog form");
    if (!(form instanceof HTMLFormElement)) {
      throw new Error("The first scenario dialog form was not available.");
    }
    window.staleScenarioDialogForm = form;
    window.staleScenarioDialogSubmitObserved = false;
    form.addEventListener("submit", () => {
      window.staleScenarioDialogSubmitObserved = true;
    });
  });

  await dialog.getByRole("button", { name: "Close" }).click();
  await expect(dialog).toBeHidden();
  await preparedAction.click();
  await expect(dialog).toBeVisible();
  await page.evaluate(() => {
    const staleForm = window.staleScenarioDialogForm;
    const currentForm = document.querySelector("dialog form");
    if (!(staleForm instanceof HTMLFormElement) || !(currentForm instanceof HTMLFormElement)) {
      throw new Error("The retained or replacement scenario dialog form was not available.");
    }
    if (staleForm.isConnected || currentForm.querySelector("input")?.value !== "") {
      throw new Error("The replacement scenario dialog was not distinct from the retained form.");
    }
    window.currentScenarioDialogForm = currentForm;
  });

  const staleSubmitPost = page.waitForRequest(
    (request) => new URL(request.url()).pathname === "/api/v1/scenarios/confirm",
    { timeout: 500 },
  ).then(() => true, () => false);
  await page.evaluate(() => {
    if (!(window.staleScenarioDialogForm instanceof HTMLFormElement)) {
      throw new Error("The retained scenario dialog form was not available.");
    }
    window.staleScenarioDialogForm.dispatchEvent(new SubmitEvent("submit", { cancelable: true }));
  });

  await expect.poll(() => page.evaluate(() => window.staleScenarioDialogSubmitObserved)).toBe(true);
  expect(await staleSubmitPost).toBe(false);
  expect(postedEndpoints).toEqual([]);
  await expect(dialog).toBeVisible();
  await expect(page.locator("#operation-status")).toBeHidden();
  await expect.poll(() => page.evaluate(() => {
    const currentForm = window.currentScenarioDialogForm;
    return currentForm instanceof HTMLFormElement
      && currentForm.isConnected
      && document.querySelector("dialog form") === currentForm
      && currentForm.querySelector("input")?.value === "";
  })).toBe(true);
});

test("ignores a detached old-process Scenario dialog form after a replacement dialog opens", async ({ page }) => {
  let stateResponseCount = 0;
  let notifySecondStateRequest;
  const secondStateRequest = new Promise((resolveSecondStateRequest) => {
    notifySecondStateRequest = resolveSecondStateRequest;
  });
  let releaseSecondStateResponse;
  const secondStateResponseReleased = new Promise((resolveSecondStateResponse) => {
    releaseSecondStateResponse = resolveSecondStateResponse;
  });
  const postedEndpoints = [];

  await page.route(/\/api\/v1\/catalog$/, async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(pdcCatalogResponse()),
    });
  });
  await page.route(/\/api\/v1\/state\?format=console-v1$/, async (route) => {
    stateResponseCount += 1;
    if (stateResponseCount === 2) {
      notifySecondStateRequest();
      await secondStateResponseReleased;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(preparedPdcStateResponse(stateResponseCount, {
        processIdentity: stateResponseCount === 1
          ? processIdentity
          : replacementProcessIdentity,
      })),
    });
  });
  await page.route(/\/api\/v1\/scenarios\/confirm$/, async (route) => {
    postedEndpoints.push(new URL(route.request().url()).pathname);
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: "{}",
    });
  });

  await page.goto(`${staticOrigin}/`);
  const currentSampleValue = page
    .locator(".catalog-summary__item")
    .filter({ hasText: "Current Sample" })
    .locator("dd");
  await expect(currentSampleValue).toHaveText("1");

  const pdcDetails = page.locator(`details[data-pdc-details-key="stream:${pdcStreamId}"]`);
  const preparedAction = pdcDetails.getByRole("button", { name: /^Confirm prepared/ });
  await pdcDetails.locator("summary").click();
  await preparedAction.click();

  const dialog = page.getByRole("dialog");
  const firstDialogForm = dialog.locator("form");
  await expect(dialog).toBeVisible();
  await firstDialogForm.locator("input").fill("stale operator");
  await page.evaluate(() => {
    const form = document.querySelector("dialog form");
    if (!(form instanceof HTMLFormElement)) {
      throw new Error("The first scenario dialog form was not available.");
    }
    window.staleScenarioDialogForm = form;
    window.staleScenarioDialogSubmitObserved = false;
    form.addEventListener("submit", () => {
      window.staleScenarioDialogSubmitObserved = true;
    });
  });

  await secondStateRequest;
  releaseSecondStateResponse();
  await expect(currentSampleValue).toHaveText("2");
  await expect(dialog).toBeHidden();

  const replacementPdcDetails = page.locator(
    `details[data-pdc-details-key="stream:${pdcStreamId}"]`,
  );
  if (!(await replacementPdcDetails.evaluate((element) => element.open))) {
    await replacementPdcDetails.locator("summary").click();
  }
  await replacementPdcDetails.getByRole("button", { name: /^Confirm prepared/ }).click();
  await expect(dialog).toBeVisible();
  await page.evaluate(() => {
    const staleForm = window.staleScenarioDialogForm;
    const currentForm = document.querySelector("dialog form");
    if (!(staleForm instanceof HTMLFormElement) || !(currentForm instanceof HTMLFormElement)) {
      throw new Error("The retained or replacement scenario dialog form was not available.");
    }
    if (staleForm.isConnected || currentForm.querySelector("input")?.value !== "") {
      throw new Error("The replacement scenario dialog was not distinct from the retained form.");
    }
    window.currentScenarioDialogForm = currentForm;
  });

  const staleSubmitPost = page.waitForRequest(
    (request) => new URL(request.url()).pathname === "/api/v1/scenarios/confirm",
    { timeout: 500 },
  ).then(() => true, () => false);
  await page.evaluate(() => {
    if (!(window.staleScenarioDialogForm instanceof HTMLFormElement)) {
      throw new Error("The retained scenario dialog form was not available.");
    }
    window.staleScenarioDialogForm.dispatchEvent(new SubmitEvent("submit", { cancelable: true }));
  });

  await expect.poll(() => page.evaluate(() => window.staleScenarioDialogSubmitObserved)).toBe(true);
  expect(await staleSubmitPost).toBe(false);
  expect(postedEndpoints).toEqual([]);
  await expect(dialog).toBeVisible();
  await expect(page.locator("#operation-status")).toBeHidden();
  await expect.poll(() => page.evaluate(() => {
    const currentForm = window.currentScenarioDialogForm;
    return currentForm instanceof HTMLFormElement
      && currentForm.isConnected
      && document.querySelector("dialog form") === currentForm
      && currentForm.querySelector("input")?.value === "";
  })).toBe(true);
});

for (const dialogAction of [
  { buttonName: /^Confirm prepared/, endpoint: "confirm" },
  { buttonName: /^Cancel prepared/, endpoint: "cancel" },
]) {
  test(`invalidates an open ${dialogAction.endpoint} dialog after a process restart`, async ({ page }) => {
    let stateResponseCount = 0;
    let notifySecondStateRequest;
    const secondStateRequest = new Promise((resolveSecondStateRequest) => {
      notifySecondStateRequest = resolveSecondStateRequest;
    });
    let releaseSecondStateResponse;
    const secondStateResponseReleased = new Promise((resolveSecondStateResponse) => {
      releaseSecondStateResponse = resolveSecondStateResponse;
    });
    const postedEndpoints = [];

    await page.route(/\/api\/v1\/catalog$/, async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(pdcCatalogResponse()),
      });
    });
    await page.route(/\/api\/v1\/state\?format=console-v1$/, async (route) => {
      stateResponseCount += 1;
      if (stateResponseCount === 2) {
        notifySecondStateRequest();
        await secondStateResponseReleased;
      }
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(preparedPdcStateResponse(stateResponseCount, {
          processIdentity: stateResponseCount === 1
            ? processIdentity
            : replacementProcessIdentity,
        })),
      });
    });
    await page.route(/\/api\/v1\/scenarios\/(confirm|cancel)$/, async (route) => {
      postedEndpoints.push(new URL(route.request().url()).pathname);
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: "{}",
      });
    });

    await page.goto(`${staticOrigin}/`);
    const currentSampleValue = page
      .locator(".catalog-summary__item")
      .filter({ hasText: "Current Sample" })
      .locator("dd");
    await expect(currentSampleValue).toHaveText("1");

    const pdcDetails = page.locator(`details[data-pdc-details-key="stream:${pdcStreamId}"]`);
    await pdcDetails.locator("summary").click();
    const preparedAction = pdcDetails.getByRole("button", { name: dialogAction.buttonName });
    await preparedAction.click();

    const dialog = page.getByRole("dialog");
    const dialogForm = dialog.locator("form");
    await expect(dialog).toBeVisible();
    await dialogForm.locator("input").fill("stale operator");
    await page.evaluate(() => {
      const form = document.querySelector("dialog form");
      if (!(form instanceof HTMLFormElement)) {
        throw new Error("The scenario dialog form was not available.");
      }
      window.staleScenarioDialogForm = form;
    });

    await secondStateRequest;
    releaseSecondStateResponse();
    await expect(currentSampleValue).toHaveText("2");
    expect(stateResponseCount).toBe(2);

    await expect.soft(dialog).toBeHidden();
    await page.evaluate(() => {
      if (!(window.staleScenarioDialogForm instanceof HTMLFormElement)) {
        throw new Error("The retained scenario dialog form was not available.");
      }
      window.staleScenarioDialogForm.dispatchEvent(new SubmitEvent("submit", { cancelable: true }));
    });
    await expect.poll(() => postedEndpoints).toEqual([]);
  });
}

test("closes an open PDC Scenario selector after a process-identity-only refresh", async ({ page }) => {
  let stateResponseCount = 0;
  let notifySecondStateRequest;
  const secondStateRequest = new Promise((resolveSecondStateRequest) => {
    notifySecondStateRequest = resolveSecondStateRequest;
  });
  let releaseSecondStateResponse;
  const secondStateResponseReleased = new Promise((resolveSecondStateResponse) => {
    releaseSecondStateResponse = resolveSecondStateResponse;
  });

  await page.route(/\/api\/v1\/catalog$/, async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(pdcCatalogResponse()),
    });
  });
  await page.route(/\/api\/v1\/state\?format=console-v1$/, async (route) => {
    stateResponseCount += 1;
    if (stateResponseCount === 2) {
      notifySecondStateRequest();
      await secondStateResponseReleased;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(pdcStateResponse(stateResponseCount, {
        processIdentity: stateResponseCount === 1
          ? processIdentity
          : replacementProcessIdentity,
      })),
    });
  });

  await page.goto(`${staticOrigin}/`);
  const currentSampleValue = page
    .locator(".catalog-summary__item")
    .filter({ hasText: "Current Sample" })
    .locator("dd");
  await expect(currentSampleValue).toHaveText("1");

  const pdcDetails = page.locator(`details[data-pdc-details-key="stream:${pdcStreamId}"]`);
  const pdcScenarioMenu = pdcDetails.locator(
    `select[aria-label="Run a compatible scenario for Stream ${pdcStreamId}, PDC connection ${pdcConnectionId}"]`,
  );
  await pdcDetails.locator("summary").click();
  await expect(pdcScenarioMenu).toBeEnabled();
  expect(await page.evaluate(() => CSS.supports("selector(select:open)"))).toBe(true);
  await pdcScenarioMenu.focus();
  await page.keyboard.press("Alt+ArrowDown");
  await expect.poll(
    () => pdcScenarioMenu.evaluate((element) => element.matches(":open")),
    { timeout: 1_000 },
  ).toBe(true);

  await secondStateRequest;
  releaseSecondStateResponse();
  await expect(currentSampleValue).toHaveText("2");
  expect(stateResponseCount).toBe(2);
  await expect.poll(
    () => pdcScenarioMenu.evaluate((element) => element.matches(":open")),
    { timeout: 1_000 },
  ).toBe(false);
});

test("does not restore PDC disclosure onto a replacement PDC target", async ({ page }) => {
  const replacementPdcStreamId = 1008;
  const replacementPdcConnectionId = 43;
  let stateResponseCount = 0;
  let notifySecondStateRequest;
  const secondStateRequest = new Promise((resolveSecondStateRequest) => {
    notifySecondStateRequest = resolveSecondStateRequest;
  });
  let releaseSecondStateResponse;
  const secondStateResponseReleased = new Promise((resolveSecondStateResponse) => {
    releaseSecondStateResponse = resolveSecondStateResponse;
  });

  await page.setViewportSize({ width: 1024, height: 768 });
  await page.route(/\/api\/v1\/catalog$/, async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(pdcCatalogResponse()),
    });
  });
  await page.route(/\/api\/v1\/state\?format=console-v1$/, async (route) => {
    stateResponseCount += 1;
    if (stateResponseCount === 2) {
      notifySecondStateRequest();
      await secondStateResponseReleased;
    }
    const pdcTarget = stateResponseCount === 1
      ? { streamId: replacementPdcStreamId, connectionId: pdcConnectionId }
      : {
        processIdentity: replacementProcessIdentity,
        streamId: replacementPdcStreamId,
        connectionId: replacementPdcConnectionId,
      };
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(pdcStateResponse(stateResponseCount, pdcTarget)),
    });
  });

  await page.goto(`${staticOrigin}/`);
  const currentSampleValue = page
    .locator(".catalog-summary__item")
    .filter({ hasText: "Current Sample" })
    .locator("dd");
  await expect(currentSampleValue).toHaveText("1");

  const pdcDetails = page.locator(`details[data-pdc-details-key="stream:${replacementPdcStreamId}"]`);
  await pdcDetails.locator("summary").click();
  await expect(pdcDetails).toHaveJSProperty("open", true);

  await secondStateRequest;
  releaseSecondStateResponse();
  await expect(currentSampleValue).toHaveText("2");
  expect(stateResponseCount).toBe(2);

  const replacementPdcDetails = page.locator(
    `details[data-pdc-details-key="stream:${replacementPdcStreamId}"]`,
  );
  const replacementPdcScenarioMenu = replacementPdcDetails.locator(
    `select[aria-label="Run a compatible scenario for Stream ${replacementPdcStreamId}, PDC connection ${replacementPdcConnectionId}"]`,
  );
  await expect(replacementPdcDetails).toHaveJSProperty("open", false);
  await expect(replacementPdcScenarioMenu).toBeEnabled();
});

test("keeps an open PDC disclosure without nested horizontal scroll through a scheduled successful refresh", async ({ page }) => {
  let stateResponseCount = 0;
  let notifySecondStateRequest;
  const secondStateRequest = new Promise((resolveSecondStateRequest) => {
    notifySecondStateRequest = resolveSecondStateRequest;
  });
  let releaseSecondStateResponse;
  const secondStateResponseReleased = new Promise((resolveSecondStateResponse) => {
    releaseSecondStateResponse = resolveSecondStateResponse;
  });

  await page.setViewportSize({ width: 1024, height: 768 });
  await page.route(/\/api\/v1\/catalog$/, async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(pdcCatalogResponse()),
    });
  });
  await page.route(/\/api\/v1\/state\?format=console-v1$/, async (route) => {
    stateResponseCount += 1;
    if (stateResponseCount === 2) {
      notifySecondStateRequest();
      await secondStateResponseReleased;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(pdcStateResponse(stateResponseCount)),
    });
  });

  await page.goto(`${staticOrigin}/`);
  const currentSampleValue = page
    .locator(".catalog-summary__item")
    .filter({ hasText: "Current Sample" })
    .locator("dd");
  await expect(currentSampleValue).toHaveText("1");

  const pdcDetails = page.locator(`details[data-pdc-details-key="stream:${pdcStreamId}"]`);
  const pdcScenarioMenu = pdcDetails.getByRole("combobox", {
    name: `Run a compatible scenario for Stream ${pdcStreamId}, PDC connection ${pdcConnectionId}`,
  });
  const pdcConnection = pdcDetails.locator(".pdc-connection");
  const pdcFields = pdcConnection.locator(".pdc-connection__field");
  const pdcFieldLabels = pdcConnection.locator("dt");
  const pdcFieldValues = pdcConnection.locator("dd");
  const pmuTableScroll = page.locator(".pmu-table").locator("xpath=..");

  await pdcDetails.locator("summary").click();
  await expect(pdcDetails).toHaveJSProperty("open", true);

  const outerScrollState = await pmuTableScroll.evaluate((element) => {
    element.scrollTo({ left: element.scrollWidth, top: element.scrollHeight });
    return { left: element.scrollLeft, top: element.scrollTop };
  });
  expect(outerScrollState.left).toBeGreaterThan(0);
  expect(outerScrollState.top).toBeGreaterThan(0);

  await expect(pdcConnection).toHaveCount(1);
  await expect(pdcFields).toHaveCount(4);
  await expect(pdcFieldLabels).toHaveText([
    "Connection ID",
    "State",
    "Streaming",
    "Scenario Controls",
  ]);
  await expect(pdcFieldValues.nth(0)).toHaveText(String(pdcConnectionId));
  await expect(pdcFieldValues.nth(1)).toHaveText("Streaming");
  await expect(pdcFieldValues.nth(2)).toHaveText("Yes");
  await expect(pdcFieldValues.nth(3)).toContainText("Run scenario...");
  await expect(pdcDetails.locator(".pdc-table-scroll")).toHaveCount(0);
  await expect(pdcDetails.locator(".pdc-table")).toHaveCount(0);
  for (let fieldIndex = 0; fieldIndex < 4; fieldIndex += 1) {
    await expect(pdcFields.nth(fieldIndex)).toBeInViewport();
  }
  await expect.poll(
    () => pdcConnection.evaluate((element) => element.scrollWidth <= element.clientWidth),
  ).toBe(true);
  await expect.poll(
    () => page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth),
  ).toBe(true);
  await expect(pdcScenarioMenu).toBeEnabled();
  await expect(pdcScenarioMenu).toBeInViewport();
  await expect(pdcScenarioMenu.locator('option[value="disconnect-pdc"]')).toHaveText("disconnect-pdc");

  await secondStateRequest;
  releaseSecondStateResponse();
  await expect(currentSampleValue).toHaveText("2");
  expect(stateResponseCount).toBe(2);

  await expect(pdcDetails).toHaveJSProperty("open", true);
  await expect(pdcFields).toHaveCount(4);
  await expect(pdcFieldLabels).toHaveText([
    "Connection ID",
    "State",
    "Streaming",
    "Scenario Controls",
  ]);
  await expect(pdcFieldValues.nth(0)).toHaveText(String(pdcConnectionId));
  await expect(pdcFieldValues.nth(1)).toHaveText("Streaming");
  await expect(pdcFieldValues.nth(2)).toHaveText("Yes");
  await expect(pdcDetails.locator(".pdc-table-scroll")).toHaveCount(0);
  await expect.poll(
    () => pdcConnection.evaluate((element) => element.scrollWidth <= element.clientWidth),
  ).toBe(true);
  await expect(pdcScenarioMenu).toBeEnabled();
  await expect(pdcScenarioMenu).toBeInViewport();
  await expect(pdcScenarioMenu.locator('option[value="disconnect-pdc"]')).toHaveText("disconnect-pdc");
  await expect.poll(() => pmuTableScroll.evaluate((element) => ({
    left: element.scrollLeft,
    top: element.scrollTop,
  }))).toEqual(outerScrollState);
});

test("keeps a maximum-safe PDC Connection ID within the four-field disclosure", async ({ page }) => {
  const longPdcConnectionId = 9007199254740991;

  await page.setViewportSize({ width: 1024, height: 768 });
  await page.route(/\/api\/v1\/catalog$/, async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(pdcCatalogResponse()),
    });
  });
  await page.route(/\/api\/v1\/state\?format=console-v1$/, async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(pdcStateResponse(1, { connectionId: longPdcConnectionId })),
    });
  });

  await page.goto(`${staticOrigin}/`);
  const pdcDetails = page.locator(`details[data-pdc-details-key="stream:${pdcStreamId}"]`);
  const pdcConnection = pdcDetails.locator(".pdc-connection");
  const pdcFields = pdcConnection.locator(".pdc-connection__field");
  const pdcFieldLabels = pdcConnection.locator("dt");
  const connectionIdField = pdcFields.nth(0);
  const connectionIdValue = connectionIdField.locator("dd");

  await pdcDetails.locator("summary").click();
  await expect(pdcConnection).toHaveCount(1);
  await expect(pdcFields).toHaveCount(4);
  await expect(pdcFieldLabels).toHaveText([
    "Connection ID",
    "State",
    "Streaming",
    "Scenario Controls",
  ]);
  await expect(connectionIdValue).toHaveText(String(longPdcConnectionId));

  await expect.poll(
    () => pdcConnection.evaluate((element) => element.scrollWidth <= element.clientWidth),
  ).toBe(true);
  await expect.poll(
    () => connectionIdField.evaluate((element) => element.scrollWidth <= element.clientWidth),
  ).toBe(true);
  await expect.poll(
    () => page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth),
  ).toBe(true);

  const connectionIdGeometry = await connectionIdValue.evaluate((element) => {
    const card = element.closest(".pdc-connection");
    if (card === null) {
      throw new Error("The PDC connection card was not available.");
    }
    const cardRect = card.getBoundingClientRect();
    const textRange = document.createRange();
    textRange.selectNodeContents(element);
    return {
      cardLeft: cardRect.left,
      cardRight: cardRect.right,
      textRects: [...textRange.getClientRects()].map((rect) => ({
        left: rect.left,
        right: rect.right,
      })),
    };
  });
  expect(connectionIdGeometry.textRects).not.toHaveLength(0);
  for (const textRect of connectionIdGeometry.textRects) {
    expect(textRect.left).toBeGreaterThanOrEqual(connectionIdGeometry.cardLeft);
    expect(textRect.right).toBeLessThanOrEqual(connectionIdGeometry.cardRight);
  }
});