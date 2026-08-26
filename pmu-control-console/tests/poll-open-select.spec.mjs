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