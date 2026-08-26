const API_PREFIX = "/api";
const POLL_INTERVAL_MS = 2_000;
const MAX_CONSOLE_PAGES = 256;
const SHA256_PATTERN = /^[a-f0-9]{64}$/;
const DECIMAL_STRING_PATTERN = /^[0-9]+$/;
const POSITIVE_DECIMAL_STRING_PATTERN = /^[1-9][0-9]*$/;
const U64_MAX_DECIMAL = "18446744073709551615";
const SCENARIO_MANAGEMENT_PATH = `${API_PREFIX}/v1/scenarios`;
const CONFIRMATION_TICK_MS = 250;
const MAX_OPERATOR_LABEL_UTF8_BYTES = 64;
const NON_RUNNABLE_SCENARIO_KINDS = new Set(["normal", "recovery"]);

class ConsoleSnapshotError extends Error {
  constructor(kind) {
    super(kind);
    this.kind = kind;
  }
}

class ScenarioRequestError extends Error {
  constructor(message) {
    super(message);
    this.message = message;
  }
}

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isSafeUnsignedInteger(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

function assertSnapshot(condition, kind = "payload") {
  if (!condition) {
    throw new ConsoleSnapshotError(kind);
  }
}

function requiredRecord(value) {
  assertSnapshot(isRecord(value));
  return value;
}

function requiredArray(value) {
  assertSnapshot(Array.isArray(value));
  return value;
}

function requiredText(value, allowEmpty = false) {
  assertSnapshot(typeof value === "string" && (allowEmpty || value.length > 0));
  return value;
}

function utf8ByteLength(value) {
  return new TextEncoder().encode(value).length;
}

function requiredUnsignedInteger(value, maximum = Number.MAX_SAFE_INTEGER) {
  assertSnapshot(isSafeUnsignedInteger(value) && value <= maximum);
  return value;
}

function requiredBoolean(value) {
  assertSnapshot(typeof value === "boolean");
  return value;
}

function requiredHash(value) {
  const hash = requiredText(value);
  assertSnapshot(SHA256_PATTERN.test(hash));
  return hash;
}

function requiredDecimalU64String(value) {
  const decimal = requiredText(value);
  assertSnapshot(DECIMAL_STRING_PATTERN.test(decimal));
  const normalized = decimal.replace(/^0+(?=[0-9])/, "");
  assertSnapshot(
    normalized.length < U64_MAX_DECIMAL.length
      || (
        normalized.length === U64_MAX_DECIMAL.length
        && normalized <= U64_MAX_DECIMAL
      ),
  );
  return normalized;
}

function requiredPositiveDecimalU64String(value) {
  const decimal = requiredText(value);
  assertSnapshot(POSITIVE_DECIMAL_STRING_PATTERN.test(decimal));
  assertSnapshot(
    decimal.length < U64_MAX_DECIMAL.length
      || (
        decimal.length === U64_MAX_DECIMAL.length
        && decimal <= U64_MAX_DECIMAL
      ),
  );
  return decimal;
}

function decimalStringIsGreaterThan(left, right) {
  return left.length > right.length || (left.length === right.length && left > right);
}

function optionalUnsignedInteger(value) {
  return value === null ? null : requiredUnsignedInteger(value);
}

function optionalText(value) {
  return value === null ? null : requiredText(value, true);
}

function requiredFiniteNumber(value) {
  assertSnapshot(typeof value === "number" && Number.isFinite(value));
  return value;
}

function parseConsoleCursor(value) {
  const cursor = requiredText(value);
  const parts = cursor.split(":");
  assertSnapshot(parts.length === 3 && SHA256_PATTERN.test(parts[0]));
  return {
    value: cursor,
    processIdentity: parts[0],
    controllerRevision: requiredDecimalU64String(parts[1]),
    offset: requiredDecimalU64String(parts[2]),
  };
}

function skipJsonWhitespace(text, position) {
  while (position < text.length && " \t\r\n".includes(text[position])) {
    position += 1;
  }
  return position;
}

function jsonStringEnd(text, position) {
  assertSnapshot(text[position] === '"');
  position += 1;
  while (position < text.length) {
    if (text[position] === "\\") {
      position += 2;
    } else if (text[position] === '"') {
      return position + 1;
    } else {
      position += 1;
    }
  }
  throw new ConsoleSnapshotError("payload");
}

function jsonValueEnd(text, position) {
  if (text[position] === '"') {
    return jsonStringEnd(text, position);
  }
  if (text[position] === "{" || text[position] === "[") {
    const closingCharacters = [text[position] === "{" ? "}" : "]"];
    position += 1;
    while (position < text.length) {
      const character = text[position];
      if (character === '"') {
        position = jsonStringEnd(text, position);
        continue;
      }
      if (character === "{") {
        closingCharacters.push("}");
      } else if (character === "[") {
        closingCharacters.push("]");
      } else if (character === "}" || character === "]") {
        assertSnapshot(character === closingCharacters[closingCharacters.length - 1]);
        closingCharacters.pop();
        position += 1;
        if (closingCharacters.length === 0) {
          return position;
        }
        continue;
      }
      position += 1;
    }
    throw new ConsoleSnapshotError("payload");
  }

  while (position < text.length && !" \t\r\n,]}".includes(text[position])) {
    position += 1;
  }
  return position;
}

function extractTopLevelDecimalU64Property(text, propertyName) {
  let position = skipJsonWhitespace(text, 0);
  assertSnapshot(text[position] === "{");
  position += 1;
  let decimalValue = null;

  for (;;) {
    position = skipJsonWhitespace(text, position);
    if (text[position] === "}") {
      assertSnapshot(decimalValue !== null);
      return decimalValue;
    }

    const keyStart = position;
    position = jsonStringEnd(text, position);
    const key = JSON.parse(text.slice(keyStart, position));
    position = skipJsonWhitespace(text, position);
    assertSnapshot(text[position] === ":");
    position = skipJsonWhitespace(text, position + 1);
    const valueStart = position;
    const valueEnd = jsonValueEnd(text, position);

    if (key === propertyName) {
      assertSnapshot(decimalValue === null);
      decimalValue = requiredDecimalU64String(text.slice(valueStart, valueEnd));
    }

    position = skipJsonWhitespace(text, valueEnd);
    if (text[position] === ",") {
      position += 1;
      continue;
    }
    assertSnapshot(text[position] === "}");
    assertSnapshot(decimalValue !== null);
    return decimalValue;
  }
}

function normalizeCatalogScenario(value) {
  const scenario = requiredRecord(value);
  const durationFrames = Object.hasOwn(scenario, "duration_frames")
    ? optionalUnsignedInteger(scenario.duration_frames)
    : null;
  let signal = null;
  if (Object.hasOwn(scenario, "signal") && scenario.signal !== null) {
    const source = requiredRecord(scenario.signal);
    signal = {
      voltageMagnitudeDelta: requiredFiniteNumber(source.voltage_magnitude_delta),
      frequencyDeviationHz: requiredFiniteNumber(source.frequency_deviation_hz),
      rocofHzPerS: requiredFiniteNumber(source.rocof_hz_per_s),
    };
  }

  return {
    index: requiredUnsignedInteger(scenario.index, 0xffffffff),
    name: requiredText(scenario.name),
    kind: requiredText(scenario.kind),
    targetCompatibility: requiredText(scenario.target_compatibility),
    lifecycle: requiredText(scenario.lifecycle),
    startFrameOffset: requiredUnsignedInteger(scenario.start_frame_offset),
    durationFrames,
    signal,
  };
}

function normalizeCatalog(value) {
  const catalog = requiredRecord(value);
  const scenarios = requiredArray(catalog.scenarios).map(normalizeCatalogScenario);
  const scenarioIndexes = new Set();
  for (const scenario of scenarios) {
    assertSnapshot(!scenarioIndexes.has(scenario.index));
    scenarioIndexes.add(scenario.index);
  }

  return {
    version: requiredUnsignedInteger(catalog.version, 0xffffffff),
    contentSha256: requiredHash(catalog.content_sha256),
    scenarios,
  };
}

function normalizeTarget(value) {
  const target = requiredRecord(value);
  const keys = Object.keys(target);
  assertSnapshot(keys.length === 1);

  if (keys[0] === "endpoint") {
    const endpoint = requiredRecord(target.endpoint);
    return {
      kind: "endpoint",
      streamId: requiredUnsignedInteger(endpoint.stream_id, 0xffff),
      connectionId: null,
    };
  }

  if (keys[0] === "pdc") {
    const pdc = requiredRecord(target.pdc);
    return {
      kind: "pdc",
      streamId: requiredUnsignedInteger(pdc.stream_id, 0xffff),
      connectionId: requiredUnsignedInteger(pdc.connection_id),
    };
  }

  throw new ConsoleSnapshotError("payload");
}

function normalizeAction(value) {
  if (value === "clear") {
    return { kind: "clear", scenarioName: null };
  }

  const action = requiredRecord(value);
  const keys = Object.keys(action);
  assertSnapshot(keys.length === 1 && keys[0] === "activate");
  const activate = requiredRecord(action.activate);
  return { kind: "activate", scenarioName: requiredText(activate.scenario_name) };
}

function normalizePreparedScenario(value) {
  const record = requiredRecord(value);
  return {
    token: requiredPositiveDecimalU64String(record.token),
    confirmExpiresInMs: requiredUnsignedInteger(record.confirm_expires_in_ms),
    target: normalizeTarget(record.target),
    action: normalizeAction(record.action),
    actorLabel: optionalText(record.actor_label),
  };
}

function normalizePendingScenario(value) {
  const record = requiredRecord(value);
  optionalText(record.actor_label);
  return {
    target: normalizeTarget(record.target),
    action: normalizeAction(record.action),
  };
}

function normalizeActiveScenario(value) {
  const record = requiredRecord(value);
  optionalText(record.actor_label);
  return {
    target: normalizeTarget(record.target),
    scenarioName: requiredText(record.scenario_name),
    kind: requiredText(record.kind),
    lifecycle: requiredText(record.lifecycle),
    startFrameOffset: requiredUnsignedInteger(record.start_frame_offset),
    firstEligibleBoundary: requiredUnsignedInteger(record.first_eligible_boundary),
  };
}

function normalizeScenarioController(value) {
  const controller = requiredRecord(value);
  return {
    currentSampleIndex: optionalUnsignedInteger(controller.current_sample_index),
    prepared: requiredArray(controller.prepared).map(normalizePreparedScenario),
    pending: requiredArray(controller.pending).map(normalizePendingScenario),
    active: requiredArray(controller.active).map(normalizeActiveScenario),
    preparedCount: requiredUnsignedInteger(controller.prepared_count),
    pendingCount: requiredUnsignedInteger(controller.pending_count),
    activeCount: requiredUnsignedInteger(controller.active_count),
  };
}

function normalizeRuntimeMetadata(value) {
  if (value === undefined) {
    return null;
  }

  const metadata = requiredRecord(value);
  const identity = requiredRecord(metadata.runtime_identity);
  return {
    deploymentLabel: requiredText(metadata.deployment_label),
    imageRef: requiredText(identity.image_ref),
    profileSha256: requiredHash(identity.profile_sha256),
    scenarioCatalogSha256: requiredHash(identity.scenario_catalog_sha256),
  };
}

function normalizeStats(value) {
  const stats = requiredRecord(value);
  const fields = [
    "accepted_clients",
    "closed_clients",
    "rejected_clients",
    "malformed_commands",
    "unsupported_commands",
    "slow_clients",
    "sent_data_frames",
    "skipped_ticks",
  ];
  const normalized = {};
  for (const field of fields) {
    normalized[field] = requiredUnsignedInteger(stats[field]);
  }
  return normalized;
}

function normalizeFleet(value) {
  const fleet = requiredRecord(value);
  const wireVersion = requiredUnsignedInteger(fleet.wire_version, 0xff);
  assertSnapshot(wireVersion === 2 || wireVersion === 3);
  const firstPort = requiredUnsignedInteger(fleet.first_port, 0xffff);
  assertSnapshot(firstPort > 0);
  return {
    pdcName: requiredText(fleet.pdc_name, true),
    pmuNamePrefix: requiredText(fleet.pmu_name_prefix),
    wireVersion,
    reportingRateHz: requiredUnsignedInteger(fleet.reporting_rate_hz, 0xffff),
    nominalFrequencyHz: requiredUnsignedInteger(fleet.nominal_frequency_hz, 0xffff),
    firstStreamId: requiredUnsignedInteger(fleet.first_stream_id, 0xffff),
    firstPmuId: requiredUnsignedInteger(fleet.first_pmu_id, 0xffff),
    firstPort,
  };
}

function normalizeEndpoint(value) {
  const endpoint = requiredRecord(value);
  const connections = requiredArray(endpoint.connections).map((connectionValue) => {
    const connection = requiredRecord(connectionValue);
    return {
      connectionId: requiredUnsignedInteger(connection.connection_id),
      streaming: requiredBoolean(connection.streaming),
    };
  });
  const connectionIds = new Set();
  for (const connection of connections) {
    assertSnapshot(!connectionIds.has(connection.connectionId));
    connectionIds.add(connection.connectionId);
  }
  connections.sort((left, right) => left.connectionId - right.connectionId);

  const activeConnections = requiredUnsignedInteger(endpoint.active_connections);
  assertSnapshot(activeConnections === connections.length);
  return {
    streamId: requiredUnsignedInteger(endpoint.stream_id, 0xffff),
    activeConnections,
    connections,
  };
}

function normalizeEndpoints(value, fleet) {
  const endpoints = requiredArray(value).map(normalizeEndpoint);
  const streamIds = new Set();
  const connectionIds = new Set();
  for (const endpoint of endpoints) {
    assertSnapshot(!streamIds.has(endpoint.streamId));
    streamIds.add(endpoint.streamId);
    const offset = endpoint.streamId - fleet.firstStreamId;
    assertSnapshot(offset >= 0);
    assertSnapshot(fleet.firstPmuId + offset <= 0xffff);
    assertSnapshot(fleet.firstPort + offset <= 0xffff);
    for (const connection of endpoint.connections) {
      assertSnapshot(!connectionIds.has(connection.connectionId));
      connectionIds.add(connection.connectionId);
    }
  }
  endpoints.sort((left, right) => left.streamId - right.streamId);
  return endpoints;
}

function normalizeConsolePage(value, rawControllerRevision) {
  const page = requiredRecord(value);
  assertSnapshot(page.format === "console-v1");
  const fleet = normalizeFleet(page.fleet);
  const processIdentity = requiredHash(page.process_identity);
  const controllerRevision = requiredDecimalU64String(rawControllerRevision);
  const nextCursor = page.next_cursor === null ? null : parseConsoleCursor(page.next_cursor);
  if (nextCursor !== null) {
    assertSnapshot(
      nextCursor.processIdentity === processIdentity
        && nextCursor.controllerRevision === controllerRevision,
    );
  }

  return {
    processIdentity,
    controllerRevision,
    catalogContentSha256: requiredHash(page.catalog_content_sha256),
    ready: requiredBoolean(page.ready),
    timeHealth: requiredText(page.time_health),
    stats: normalizeStats(page.stats),
    fleet,
    endpoints: normalizeEndpoints(page.endpoints, fleet),
    runtimeMetadata: normalizeRuntimeMetadata(page.runtime_metadata),
    scenarioController: normalizeScenarioController(page.scenario_controller),
    nextCursor,
  };
}

function stablePageFingerprint(page) {
  return JSON.stringify({
    ready: page.ready,
    timeHealth: page.timeHealth,
    fleet: page.fleet,
    endpoints: page.endpoints,
    runtimeMetadata: page.runtimeMetadata,
  });
}

function targetKey(target) {
  return target.kind === "pdc"
    ? `pdc:${target.streamId}:${target.connectionId}`
    : `endpoint:${target.streamId}`;
}

function targetPayload(target) {
  return target.kind === "pdc"
    ? { stream_id: target.streamId, connection_id: target.connectionId }
    : { stream_id: target.streamId };
}

function targetsMatch(left, right) {
  return targetKey(left) === targetKey(right);
}

function targetDescription(target) {
  if (target.kind === "pdc") {
    return `Stream ${target.streamId}, PDC connection ${target.connectionId}`;
  }
  return `Stream ${target.streamId}`;
}

function actionDescription(action) {
  return action.kind === "clear" ? "Clear active scenario" : action.scenarioName;
}

function normalizedTargetCompatibility(value) {
  return value.toLowerCase().replace(/[ -]/g, "_");
}

function scenarioIsRunnableForTarget(scenario, target) {
  if (NON_RUNNABLE_SCENARIO_KINDS.has(scenario.kind.toLowerCase())) {
    return false;
  }
  const compatibility = normalizedTargetCompatibility(scenario.targetCompatibility);
  if (target.kind === "endpoint") {
    return compatibility === "endpoint" || compatibility === "endpoint_only";
  }
  return compatibility === "pdc" || compatibility === "pdc_connection" || compatibility === "connection";
}

function scenariosForTarget(snapshot, target) {
  return snapshot.catalog.scenarios.filter((scenario) => scenarioIsRunnableForTarget(scenario, target));
}

function recordsForTarget(records, target) {
  return records.filter((record) => targetsMatch(record.target, target));
}

function targetHasLivePdcConnection(snapshot, target) {
  if (target.kind !== "pdc") {
    return true;
  }
  const endpoint = snapshot.endpoints.find(
    (candidate) => candidate.streamId === target.streamId,
  );
  return endpoint !== undefined
    && endpoint.connections.some((connection) => connection.connectionId === target.connectionId);
}

function targetScenarioAvailability(snapshot, target) {
  const prepared = recordsForTarget(snapshot.scenarioController.prepared, target);
  const pending = recordsForTarget(snapshot.scenarioController.pending, target);
  const active = recordsForTarget(snapshot.scenarioController.active, target);
  const pdcLive = targetHasLivePdcConnection(snapshot, target);
  const busy = prepared.length > 0 || pending.length > 0 || active.length > 0;
  const hasSustainedActiveScenario = active.some(
    (record) => record.lifecycle.toLowerCase() === "sustained",
  );
  return {
    prepared,
    pending,
    active,
    pdcLive,
    busy,
    runAvailable: pdcLive && !busy,
    clearAvailable: hasSustainedActiveScenario && prepared.length === 0 && pending.length === 0,
  };
}

function activeScenarioFocusKey(record) {
  return `scenario-clear:${targetKey(record.target)}:${record.scenarioName}:${record.startFrameOffset}`;
}

function confirmationExpiresAt(record) {
  if (!Number.isFinite(record.confirmExpiresAtMs)) {
    record.confirmExpiresAtMs = Date.now() + record.confirmExpiresInMs;
  }
  return record.confirmExpiresAtMs;
}

function formatConfirmationCountdown(expiresAt, now = Date.now()) {
  const remainingSeconds = Math.max(0, Math.ceil((expiresAt - now) / 1_000));
  const minutes = Math.floor(remainingSeconds / 60);
  const seconds = String(remainingSeconds % 60).padStart(2, "0");
  return `${minutes}:${seconds}`;
}

function assertUniqueLifecycleRecords(controller) {
  const preparedTokens = new Set();
  const pendingRecords = new Set();
  const activeRecords = new Set();

  for (const record of controller.prepared) {
    assertSnapshot(!preparedTokens.has(record.token), "coherence");
    preparedTokens.add(record.token);
  }
  for (const record of controller.pending) {
    const key = `${targetKey(record.target)}:${record.action.kind}:${record.action.scenarioName ?? ""}`;
    assertSnapshot(!pendingRecords.has(key), "coherence");
    pendingRecords.add(key);
  }
  for (const record of controller.active) {
    const key = `${targetKey(record.target)}:${record.scenarioName}:${record.startFrameOffset}`;
    assertSnapshot(!activeRecords.has(key), "coherence");
    activeRecords.add(key);
  }
}

function assertTargetsMatchEndpoints(controller, endpoints) {
  const streamIds = new Set(endpoints.map((endpoint) => endpoint.streamId));
  for (const record of [...controller.prepared, ...controller.pending, ...controller.active]) {
    assertSnapshot(streamIds.has(record.target.streamId), "coherence");
  }
}

function errorCodeFromPayload(payload) {
  if (!isRecord(payload) || !isRecord(payload.error) || typeof payload.error.code !== "string") {
    return null;
  }
  return payload.error.code;
}

async function requestJson(url) {
  let response;
  try {
    response = await fetch(url, {
      method: "GET",
      headers: { Accept: "application/json" },
      cache: "no-store",
    });
  } catch {
    throw new ConsoleSnapshotError("network");
  }

  let text;
  try {
    text = await response.text();
  } catch {
    throw new ConsoleSnapshotError("response");
  }

  let payload;
  try {
    payload = JSON.parse(text);
  } catch {
    throw new ConsoleSnapshotError("payload");
  }

  if (!response.ok) {
    if (errorCodeFromPayload(payload) === "stale_console_cursor") {
      throw new ConsoleSnapshotError("stale_cursor");
    }
    throw new ConsoleSnapshotError("response");
  }
  return { payload, text };
}

function scenarioErrorMessage(payload, fallback) {
  if (!isRecord(payload)) {
    return fallback;
  }
  if (typeof payload.message === "string" && payload.message !== "") {
    return payload.message;
  }
  if (!isRecord(payload.error)) {
    return fallback;
  }
  if (typeof payload.error.message === "string" && payload.error.message !== "") {
    return payload.error.message;
  }
  if (typeof payload.error.code === "string" && payload.error.code !== "") {
    return `The management plane rejected the request: ${humanizeIdentifier(payload.error.code)}.`;
  }
  return fallback;
}

async function postScenarioAction(action, body) {
  let response;
  try {
    response = await fetch(`${SCENARIO_MANAGEMENT_PATH}/${action}`, {
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
      },
      body: JSON.stringify(body),
      cache: "no-store",
    });
  } catch {
    throw new ScenarioRequestError("The scenario management plane could not be reached.");
  }

  let text;
  try {
    text = await response.text();
  } catch {
    throw new ScenarioRequestError("The scenario management plane returned an unreadable response.");
  }

  let payload = null;
  if (text.trim() !== "") {
    try {
      payload = JSON.parse(text);
    } catch {
      throw new ScenarioRequestError(response.ok
        ? "The scenario management plane returned an invalid response."
        : `The scenario management plane rejected the request with HTTP ${response.status}.`);
    }
  }

  if (!response.ok) {
    throw new ScenarioRequestError(scenarioErrorMessage(
      payload,
      `The scenario management plane rejected the request with HTTP ${response.status}.`,
    ));
  }
  return payload;
}

async function fetchCatalog() {
  const { payload } = await requestJson(`${API_PREFIX}/v1/catalog`);
  return normalizeCatalog(payload);
}

function stateUrl(cursor) {
  if (cursor === null) {
    return `${API_PREFIX}/v1/state?format=console-v1`;
  }
  const validatedCursor = parseConsoleCursor(cursor);
  return `${API_PREFIX}/v1/state?format=console-v1&cursor=${validatedCursor.value}`;
}

async function fetchConsolePage(cursor) {
  const { payload, text } = await requestJson(stateUrl(cursor));
  return normalizeConsolePage(
    payload,
    extractTopLevelDecimalU64Property(text, "controller_revision"),
  );
}

async function collectConsolePages(catalog) {
  let cursor = null;
  let baseline = null;
  let baselineFingerprint = null;
  let expectedCounts = null;
  const seenCursors = new Set();
  const lifecycle = { prepared: [], pending: [], active: [] };

  for (let pageNumber = 0; pageNumber < MAX_CONSOLE_PAGES; pageNumber += 1) {
    if (cursor !== null) {
      assertSnapshot(!seenCursors.has(cursor.value), "coherence");
      seenCursors.add(cursor.value);
    }

    const page = await fetchConsolePage(cursor === null ? null : cursor.value);
    if (page.catalogContentSha256 !== catalog.contentSha256) {
      throw new ConsoleSnapshotError("catalog_mismatch");
    }

    if (baseline === null) {
      baseline = page;
      baselineFingerprint = stablePageFingerprint(page);
      expectedCounts = {
        prepared: page.scenarioController.preparedCount,
        pending: page.scenarioController.pendingCount,
        active: page.scenarioController.activeCount,
      };
    } else {
      assertSnapshot(page.processIdentity === baseline.processIdentity, "coherence");
      assertSnapshot(page.controllerRevision === baseline.controllerRevision, "coherence");
      assertSnapshot(page.catalogContentSha256 === baseline.catalogContentSha256, "catalog_mismatch");
      assertSnapshot(stablePageFingerprint(page) === baselineFingerprint, "coherence");
      assertSnapshot(
        page.scenarioController.preparedCount === expectedCounts.prepared
          && page.scenarioController.pendingCount === expectedCounts.pending
          && page.scenarioController.activeCount === expectedCounts.active,
        "coherence",
      );
    }

    lifecycle.prepared.push(...page.scenarioController.prepared);
    lifecycle.pending.push(...page.scenarioController.pending);
    lifecycle.active.push(...page.scenarioController.active);

    if (page.nextCursor === null) {
      assertSnapshot(
        lifecycle.prepared.length === expectedCounts.prepared
          && lifecycle.pending.length === expectedCounts.pending
          && lifecycle.active.length === expectedCounts.active,
        "coherence",
      );
      const scenarioController = {
        currentSampleIndex: baseline.scenarioController.currentSampleIndex,
        ...lifecycle,
        preparedCount: expectedCounts.prepared,
        pendingCount: expectedCounts.pending,
        activeCount: expectedCounts.active,
      };
      assertUniqueLifecycleRecords(scenarioController);
      assertTargetsMatchEndpoints(scenarioController, baseline.endpoints);
      return { ...baseline, catalog, scenarioController };
    }

    assertSnapshot(
      decimalStringIsGreaterThan(page.nextCursor.offset, cursor === null ? "0" : cursor.offset),
      "coherence",
    );
    assertSnapshot(!seenCursors.has(page.nextCursor.value), "coherence");
    cursor = page.nextCursor;
  }

  throw new ConsoleSnapshotError("coherence");
}

async function loadCoherentSnapshot() {
  let catalog = await fetchCatalog();
  let catalogRetried = false;
  let staleCursorRetried = false;

  for (;;) {
    try {
      return await collectConsolePages(catalog);
    } catch (error) {
      const snapshotError = error instanceof ConsoleSnapshotError
        ? error
        : new ConsoleSnapshotError("response");
      if (snapshotError.kind === "catalog_mismatch" && !catalogRetried) {
        catalogRetried = true;
        staleCursorRetried = false;
        catalog = await fetchCatalog();
        continue;
      }
      if (snapshotError.kind === "stale_cursor" && !staleCursorRetried) {
        staleCursorRetried = true;
        continue;
      }
      throw snapshotError;
    }
  }
}

function humanizeIdentifier(value) {
  return value.replace(/_/g, " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function formatTimestamp(value) {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "medium",
  }).format(value);
}

function formatOptionalText(value) {
  return value === "" ? "--" : value;
}

function formatSignal(signal) {
  if (signal === null) {
    return "--";
  }
  return [
    `V ${signal.voltageMagnitudeDelta}`,
    `f ${signal.frequencyDeviationHz} Hz`,
    `ROCOF ${signal.rocofHzPerS} Hz/s`,
  ].join("; ");
}

function endpointView(endpoint, snapshot) {
  const offset = endpoint.streamId - snapshot.fleet.firstStreamId;
  const activeScenarioNames = new Set();
  for (const activeScenario of snapshot.scenarioController.active) {
    if (activeScenario.target.streamId === endpoint.streamId) {
      activeScenarioNames.add(activeScenario.scenarioName);
    }
  }
  return {
    ...endpoint,
    pmuId: snapshot.fleet.firstPmuId + offset,
    port: snapshot.fleet.firstPort + offset,
    pmuName: `${snapshot.fleet.pmuNamePrefix}${String(offset + 1).padStart(3, "0")}`,
    pdcName: snapshot.fleet.pdcName,
    wireVersion: snapshot.fleet.wireVersion,
    reportingRateHz: snapshot.fleet.reportingRateHz,
    nominalFrequencyHz: snapshot.fleet.nominalFrequencyHz,
    timeHealth: snapshot.timeHealth,
    activeScenarioNames: [...activeScenarioNames].sort((left, right) => left.localeCompare(right)),
  };
}

function buildFilterOptions(endpointViews) {
  const wireVersions = [...new Set(endpointViews.map((endpoint) => endpoint.wireVersion))]
    .sort((left, right) => left - right);
  const occupancies = [...new Set(endpointViews.map((endpoint) => endpoint.activeConnections))]
    .sort((left, right) => left - right);
  const activeScenarios = [...new Set(endpointViews.flatMap((endpoint) => endpoint.activeScenarioNames))]
    .sort((left, right) => left.localeCompare(right));
  const timeHealth = [...new Set(endpointViews.map((endpoint) => endpoint.timeHealth))]
    .sort((left, right) => left.localeCompare(right));
  return { wireVersions, occupancies, activeScenarios, timeHealth };
}

function normalizeFilterValue(value, options) {
  return options.includes(value) ? value : "";
}

function filterEndpoints(endpointViews, filters) {
  const streamQuery = filters.streamId.trim();
  return endpointViews.filter((endpoint) => {
    return (
      (streamQuery === "" || String(endpoint.streamId).includes(streamQuery))
      && (filters.wireVersion === "" || String(endpoint.wireVersion) === filters.wireVersion)
      && (filters.occupancy === "" || String(endpoint.activeConnections) === filters.occupancy)
      && (filters.activeScenario === "" || endpoint.activeScenarioNames.includes(filters.activeScenario))
      && (filters.timeHealth === "" || endpoint.timeHealth === filters.timeHealth)
    );
  });
}

function createElement(tagName, options = {}) {
  const element = document.createElement(tagName);
  if (options.className) {
    element.className = options.className;
  }
  if (options.text !== undefined) {
    element.textContent = options.text;
  }
  if (options.attributes) {
    for (const [name, value] of Object.entries(options.attributes)) {
      if (value !== false && value !== null && value !== undefined) {
        element.setAttribute(name, value === true ? "" : String(value));
      }
    }
  }
  return element;
}

function appendChildren(parent, children) {
  for (const child of children) {
    if (child !== null && child !== undefined) {
      parent.append(child);
    }
  }
  return parent;
}

function createSummaryItem(label, value, valueClassName = "") {
  const item = createElement("div", { className: "catalog-summary__item" });
  const term = createElement("dt", { text: label });
  const description = createElement("dd", {
    className: valueClassName,
    text: value,
  });
  return appendChildren(item, [term, description]);
}

function createCell(text, className = "") {
  return createElement("td", { className, text });
}

function createEmptyValueCell() {
  const cell = createElement("td");
  cell.append(createElement("span", { className: "empty-value", text: "--" }));
  return cell;
}

function createCatalogSection(snapshot) {
  const section = createElement("section", {
    className: "console-section",
    attributes: { "aria-labelledby": "catalog-title" },
  });
  const heading = createElement("div", { className: "section-heading" });
  const headingText = createElement("div");
  appendChildren(headingText, [
    createElement("p", { className: "eyebrow", text: "Catalog" }),
    createElement("h2", { text: "Scenario Catalog", attributes: { id: "catalog-title" } }),
  ]);
  heading.append(headingText);

  const summary = createElement("dl", { className: "catalog-summary" });
  appendChildren(summary, [
    createSummaryItem("Version", String(snapshot.catalog.version), "numeric-cell"),
    createSummaryItem("Scenarios", String(snapshot.catalog.scenarios.length), "numeric-cell"),
    createSummaryItem("Content SHA-256", snapshot.catalog.contentSha256, "identity-value"),
    createSummaryItem(
      "Current Sample",
      snapshot.scenarioController.currentSampleIndex === null
        ? "--"
        : String(snapshot.scenarioController.currentSampleIndex),
      "numeric-cell",
    ),
  ]);

  const table = createElement("table", { className: "catalog-table" });
  table.append(createElement("caption", {
    className: "screen-reader-text",
    text: "Configured scenario catalog",
  }));
  const colgroup = createElement("colgroup");
  for (const width of ["6%", "19%", "12%", "12%", "11%", "12%", "10%", "18%"] ) {
    colgroup.append(createElement("col", { attributes: { style: `width: ${width}` } }));
  }
  const tableHead = createElement("thead");
  const headerRow = createElement("tr");
  for (const label of ["Index", "Scenario", "Kind", "Target", "Lifecycle", "Start Frame", "Duration", "Signal"]) {
    headerRow.append(createElement("th", { text: label, attributes: { scope: "col" } }));
  }
  tableHead.append(headerRow);
  const tableBody = createElement("tbody");
  for (const scenario of snapshot.catalog.scenarios) {
    const row = createElement("tr");
    appendChildren(row, [
      createCell(String(scenario.index), "numeric-cell"),
      createCell(scenario.name),
      createCell(humanizeIdentifier(scenario.kind)),
      createCell(humanizeIdentifier(scenario.targetCompatibility)),
      createCell(humanizeIdentifier(scenario.lifecycle)),
      createCell(String(scenario.startFrameOffset), "numeric-cell"),
      scenario.durationFrames === null
        ? createEmptyValueCell()
        : createCell(String(scenario.durationFrames), "numeric-cell"),
      createCell(formatSignal(scenario.signal)),
    ]);
    tableBody.append(row);
  }
  appendChildren(table, [colgroup, tableHead, tableBody]);
  const tableScroll = createElement("div", { className: "table-scroll" });
  tableScroll.append(table);
  appendChildren(section, [heading, summary, tableScroll]);
  return section;
}

function createSelectOption(value, text) {
  return createElement("option", { text, attributes: { value } });
}

function createFilterField(id, label, control) {
  const field = createElement("div", { className: "filter-field" });
  const fieldLabel = createElement("label", { text: label, attributes: { for: id } });
  control.id = id;
  appendChildren(field, [fieldLabel, control]);
  return field;
}

function createScenarioRunMenu(target, snapshot, controls, availability) {
  const scenarios = scenariosForTarget(snapshot, target);
  const select = createElement("select", {
    className: "scenario-run-menu",
    attributes: {
      "aria-label": `Run a compatible scenario for ${targetDescription(target)}`,
      "data-focus-key": `scenario-run:${targetKey(target)}`,
    },
  });
  if (scenarios.length === 0) {
    select.append(createSelectOption("", "No compatible scenarios"));
    select.disabled = true;
    return select;
  }

  select.setAttribute("data-management-control", "");
  select.setAttribute(
    "data-target-action-available",
    availability.runAvailable ? "true" : "false",
  );
  const placeholder = !availability.pdcLive
    ? "PDC disconnected"
    : availability.busy
      ? "Scenario action in progress"
      : "Run scenario...";
  select.append(createSelectOption("", placeholder));
  for (const scenario of scenarios) {
    select.append(createSelectOption(scenario.name, scenario.name));
  }
  select.disabled = !controls.enabled || !availability.runAvailable;
  select.addEventListener("change", () => {
    const scenario = scenarios.find((candidate) => candidate.name === select.value);
    select.value = "";
    if (scenario !== undefined) {
      controls.onRunScenario(target, scenario);
    }
  });
  return select;
}

function createPreparedScenarioState(record, controls) {
  const expiresAt = confirmationExpiresAt(record);
  const expired = expiresAt <= Date.now();
  const item = createElement("div", { className: "scenario-state scenario-state--prepared" });
  const summary = createElement("p", {
    className: "scenario-state__summary",
    text: `Prepared: ${actionDescription(record.action)}`,
  });
  const countdown = createElement("output", {
    className: "scenario-countdown",
    text: expired ? "Confirmation expired" : `Confirm within ${formatConfirmationCountdown(expiresAt)}`,
    attributes: {
      "aria-label": "Confirmation deadline",
      "data-confirm-expires-at": expiresAt,
      "data-confirm-countdown": "",
    },
  });
  item.append(summary, countdown);
  if (record.actorLabel !== null && record.actorLabel !== "") {
    item.append(createElement("p", {
      className: "scenario-state__detail",
      text: `Prepared by ${record.actorLabel}`,
    }));
  }

  const actions = createElement("div", { className: "scenario-state__actions" });
  const confirmationButton = createElement("button", {
    className: "scenario-action scenario-action--confirm",
    text: "Confirm",
    attributes: {
      type: "button",
      disabled: !controls.enabled || expired,
      "data-management-control": "",
      "data-confirm-expires-at": expiresAt,
      "data-focus-key": `scenario-confirm:${record.token}`,
      "aria-label": `Confirm prepared ${actionDescription(record.action)} for ${targetDescription(record.target)}`,
    },
  });
  confirmationButton.addEventListener("click", () => controls.onConfirmPrepared(record));
  const cancellationButton = createElement("button", {
    className: "scenario-action",
    text: "Cancel",
    attributes: {
      type: "button",
      disabled: !controls.enabled || expired,
      "data-management-control": "",
      "data-confirm-expires-at": expiresAt,
      "data-focus-key": `scenario-cancel:${record.token}`,
      "aria-label": `Cancel prepared ${actionDescription(record.action)} for ${targetDescription(record.target)}`,
    },
  });
  cancellationButton.addEventListener("click", () => controls.onCancelPrepared(record));
  actions.append(confirmationButton, cancellationButton);
  item.append(actions);
  return item;
}

function createPendingScenarioState(record) {
  return createElement("p", {
    className: "scenario-state scenario-state--pending",
    text: `Pending: ${actionDescription(record.action)}`,
  });
}

function createActiveScenarioState(target, record, controls, clearAvailable) {
  const item = createElement("div", { className: "scenario-state scenario-state--active" });
  item.append(createElement("p", {
    className: "scenario-state__summary",
    text: `Active: ${record.scenarioName}`,
  }));
  if (record.lifecycle.toLowerCase() !== "sustained" || !clearAvailable) {
    return item;
  }

  const clearButton = createElement("button", {
    className: "scenario-action scenario-action--clear",
    text: "Clear",
    attributes: {
      type: "button",
      disabled: !controls.enabled,
      "data-management-control": "",
      "data-target-action-available": clearAvailable ? "true" : "false",
      "data-focus-key": activeScenarioFocusKey(record),
      "aria-label": `Clear sustained ${record.scenarioName} for ${targetDescription(target)}`,
    },
  });
  clearButton.addEventListener("click", () => controls.onClearScenario(target, record));
  item.append(clearButton);
  return item;
}

function createTargetScenarioControls(target, snapshot, controls) {
  const container = createElement("div", { className: "target-scenario-controls" });
  const availability = targetScenarioAvailability(snapshot, target);
  container.append(createScenarioRunMenu(target, snapshot, controls, availability));

  for (const record of availability.prepared) {
    container.append(createPreparedScenarioState(record, controls));
  }
  for (const record of availability.pending) {
    container.append(createPendingScenarioState(record));
  }
  let clearAvailable = availability.clearAvailable;
  for (const record of availability.active) {
    const canClearScenario = clearAvailable && record.lifecycle.toLowerCase() === "sustained";
    container.append(createActiveScenarioState(target, record, controls, canClearScenario));
    if (canClearScenario) {
      clearAvailable = false;
    }
  }
  return container;
}

function disabledScenarioControls() {
  return {
    enabled: false,
    onRunScenario() {},
    onConfirmPrepared() {},
    onCancelPrepared() {},
    onClearScenario() {},
  };
}

function pdcDetailsKey(endpoint) {
  return `stream:${endpoint.streamId}`;
}

function pdcConnectionRowsForEndpoint(endpoint, snapshot) {
  const liveConnections = new Map(
    endpoint.connections.map((connection) => [connection.connectionId, connection]),
  );
  const connectionIds = new Set(liveConnections.keys());
  for (const records of [
    snapshot.scenarioController.prepared,
    snapshot.scenarioController.pending,
    snapshot.scenarioController.active,
  ]) {
    for (const record of records) {
      if (record.target.kind === "pdc" && record.target.streamId === endpoint.streamId) {
        connectionIds.add(record.target.connectionId);
      }
    }
  }
  return [...connectionIds]
    .sort((left, right) => left - right)
    .map((connectionId) => ({
      connectionId,
      connection: liveConnections.get(connectionId) ?? null,
    }));
}

function pdcScenarioStateForEndpoint(endpoint, snapshot) {
  const stateForPdcTargets = (records) => records.filter((record) => {
    return record.target.kind === "pdc" && record.target.streamId === endpoint.streamId;
  });
  return {
    prepared: stateForPdcTargets(snapshot.scenarioController.prepared),
    pending: stateForPdcTargets(snapshot.scenarioController.pending),
    active: stateForPdcTargets(snapshot.scenarioController.active),
  };
}

function createPdcDetailsSummary(endpoint, snapshot) {
  const noun = endpoint.activeConnections === 1 ? "connection" : "connections";
  const summary = createElement("summary", {
    attributes: {
      "data-focus-key": `pdc-summary:${pdcDetailsKey(endpoint)}`,
      "aria-label": `PDC connection details for ${endpoint.pmuName}, PMU ${endpoint.pmuId}, stream ${endpoint.streamId}`,
    },
  });
  summary.append(createElement("span", {
    className: "pdc-details__summary-label",
    text: `${endpoint.activeConnections} live ${noun}`,
  }));

  const scenarioState = pdcScenarioStateForEndpoint(endpoint, snapshot);
  const scenarioCount = scenarioState.prepared.length
    + scenarioState.pending.length
    + scenarioState.active.length;
  if (scenarioCount === 0) {
    return summary;
  }

  const stateLabels = [];
  if (scenarioState.prepared.length > 0) {
    stateLabels.push(`${scenarioState.prepared.length} prepared`);
  }
  if (scenarioState.pending.length > 0) {
    stateLabels.push(`${scenarioState.pending.length} pending`);
  }
  if (scenarioState.active.length > 0) {
    stateLabels.push(`${scenarioState.active.length} active`);
  }
  const stateClass = scenarioState.prepared.length > 0
    ? "prepared"
    : scenarioState.pending.length > 0
      ? "pending"
      : "active";
  const stateIndicator = createElement("span", {
    className: `pdc-details__scenario-count pdc-details__scenario-count--${stateClass}`,
  });
  stateIndicator.append(createElement("span", {
    text: `Scenario: ${stateLabels.join(", ")}`,
  }));
  if (scenarioState.prepared.length > 0) {
    const expiresAt = Math.min(
      ...scenarioState.prepared.map((record) => confirmationExpiresAt(record)),
    );
    stateIndicator.append(createElement("output", {
      className: "pdc-details__scenario-countdown",
      text: expiresAt <= Date.now()
        ? "Confirmation expired"
        : `Confirm within ${formatConfirmationCountdown(expiresAt)}`,
      attributes: {
        "aria-label": "Earliest PDC confirmation deadline",
        "data-confirm-expires-at": expiresAt,
        "data-confirm-countdown": "",
      },
    }));
  }
  summary.append(stateIndicator);
  return summary;
}

function createPdcDetails(endpoint, snapshot, controls) {
  const details = createElement("details", {
    className: "pdc-details",
    attributes: { "data-pdc-details-key": pdcDetailsKey(endpoint) },
  });
  const connectionRows = pdcConnectionRowsForEndpoint(endpoint, snapshot);
  details.append(createPdcDetailsSummary(endpoint, snapshot));
  if (endpoint.connections.length === 0) {
    details.append(createElement("p", { className: "pdc-empty", text: "No live PDC connections" }));
  }
  if (connectionRows.length === 0) {
    return details;
  }

  const table = createElement("table", { className: "pdc-table" });
  table.append(createElement("caption", {
    className: "screen-reader-text",
    text: `PDC connection details for stream ${endpoint.streamId}`,
  }));
  const tableHead = createElement("thead");
  const headerRow = createElement("tr");
  for (const label of ["Connection ID", "State", "Streaming", "Scenario Controls"]) {
    headerRow.append(createElement("th", { text: label, attributes: { scope: "col" } }));
  }
  tableHead.append(headerRow);
  const tableBody = createElement("tbody");
  for (const { connectionId, connection } of connectionRows) {
    const row = createElement("tr");
    const scenarioControls = createElement("td", { className: "scenario-controls-cell" });
    scenarioControls.append(createTargetScenarioControls({
      kind: "pdc",
      streamId: endpoint.streamId,
      connectionId,
    }, snapshot, controls));
    appendChildren(row, [
      createCell(String(connectionId), "numeric-cell"),
      createCell(connection === null ? "Disconnected" : connection.streaming ? "Streaming" : "Connected"),
      createCell(connection?.streaming ? "Yes" : "No"),
      scenarioControls,
    ]);
    tableBody.append(row);
  }
  appendChildren(table, [tableHead, tableBody]);
  const tableScroll = createElement("div", {
    className: "pdc-table-scroll",
    attributes: {
      tabindex: "0",
      "aria-label": `PDC connection details for stream ${endpoint.streamId}`,
      "data-focus-key": `pdc-table:${pdcDetailsKey(endpoint)}`,
    },
  });
  tableScroll.append(table);
  details.append(tableScroll);
  return details;
}

function createPmuTable(endpointViews, totalCount, snapshot, controls) {
  const table = createElement("table", { className: "pmu-table" });
  table.append(createElement("caption", {
    className: "screen-reader-text",
    text: "Live PMU stream state",
  }));
  const colgroup = createElement("colgroup");
  for (const width of ["74px", "68px", "58px", "52px", "108px", "108px", "70px", "76px", "100px", "88px", "128px", "148px", "200px"] ) {
    colgroup.append(createElement("col", { attributes: { style: `width: ${width}` } }));
  }
  const tableHead = createElement("thead");
  const headerRow = createElement("tr");
  for (const label of [
    "Stream ID",
    "PMU ID",
    "Port",
    "Wire",
    "PMU Name",
    "PDC Name",
    "Rate",
    "Nominal",
    "Time Health",
    "PDC Occupancy",
    "Active Scenario",
    "PDC Details",
    "Scenario Controls",
  ]) {
    headerRow.append(createElement("th", { text: label, attributes: { scope: "col" } }));
  }
  tableHead.append(headerRow);
  const tableBody = createElement("tbody");
  if (endpointViews.length === 0) {
    const row = createElement("tr");
    row.append(createElement("td", {
      className: "empty-value",
      text: "No PMUs match the current filters.",
      attributes: { colspan: "13" },
    }));
    tableBody.append(row);
  }
  for (const endpoint of endpointViews) {
    const row = createElement("tr");
    appendChildren(row, [
      createCell(String(endpoint.streamId), "id-cell"),
      createCell(String(endpoint.pmuId), "id-cell"),
      createCell(String(endpoint.port), "numeric-cell"),
      createCell(`V${endpoint.wireVersion}`, "numeric-cell"),
      createCell(endpoint.pmuName),
      endpoint.pdcName === ""
        ? createEmptyValueCell()
        : createCell(endpoint.pdcName),
      createCell(`${endpoint.reportingRateHz} Hz`, "rate-cell"),
      createCell(`${endpoint.nominalFrequencyHz} Hz`, "rate-cell"),
      createCell(humanizeIdentifier(endpoint.timeHealth)),
      createCell(String(endpoint.activeConnections), "numeric-cell"),
      endpoint.activeScenarioNames.length === 0
        ? createEmptyValueCell()
        : createCell(endpoint.activeScenarioNames.join(", ")),
    ]);
    const detailsCell = createElement("td");
    detailsCell.append(createPdcDetails(endpoint, snapshot, controls));
    const scenarioControls = createElement("td", { className: "scenario-controls-cell" });
    scenarioControls.append(createTargetScenarioControls({
      kind: "endpoint",
      streamId: endpoint.streamId,
      connectionId: null,
    }, snapshot, controls));
    row.append(detailsCell, scenarioControls);
    tableBody.append(row);
  }
  appendChildren(table, [colgroup, tableHead, tableBody]);
  const tableScroll = createElement("div", { className: "table-scroll" });
  tableScroll.append(table);
  return { tableScroll, countLabel: `${endpointViews.length} of ${totalCount} PMUs` };
}

function createPmuSection(snapshot, filters, onFiltersChanged, controls) {
  const endpointViews = snapshot.endpoints.map((endpoint) => endpointView(endpoint, snapshot));
  const filterOptions = buildFilterOptions(endpointViews);
  filters.wireVersion = normalizeFilterValue(filters.wireVersion, filterOptions.wireVersions.map(String));
  filters.occupancy = normalizeFilterValue(filters.occupancy, filterOptions.occupancies.map(String));
  filters.activeScenario = normalizeFilterValue(filters.activeScenario, filterOptions.activeScenarios);
  filters.timeHealth = normalizeFilterValue(filters.timeHealth, filterOptions.timeHealth);
  const filteredEndpoints = filterEndpoints(endpointViews, filters);

  function rerenderFromControl(control) {
    const focusState = { id: control.id };
    if (typeof control.selectionStart === "number" && typeof control.selectionEnd === "number") {
      focusState.selectionStart = control.selectionStart;
      focusState.selectionEnd = control.selectionEnd;
    }
    onFiltersChanged(focusState);
  }

  const section = createElement("section", {
    className: "console-section",
    attributes: { "aria-labelledby": "pmu-state-title" },
  });
  const heading = createElement("div", { className: "section-heading" });
  const headingText = createElement("div");
  appendChildren(headingText, [
    createElement("p", { className: "eyebrow", text: "Live State" }),
    createElement("h2", { text: "PMU Streams", attributes: { id: "pmu-state-title" } }),
  ]);
  const table = createPmuTable(
    filteredEndpoints,
    endpointViews.length,
    snapshot,
    controls ?? disabledScenarioControls(),
  );
  appendChildren(heading, [
    headingText,
    createElement("p", { className: "table-count", text: table.countLabel }),
  ]);

  const form = createElement("form", { className: "filters", attributes: { "aria-label": "PMU filters" } });
  form.addEventListener("submit", (event) => event.preventDefault());
  const fieldset = createElement("fieldset");
  fieldset.append(createElement("legend", { text: "PMU filters" }));

  const streamIdInput = createElement("input", {
    attributes: { type: "text", inputmode: "numeric", autocomplete: "off", value: filters.streamId },
  });
  streamIdInput.addEventListener("input", () => {
    filters.streamId = streamIdInput.value;
    rerenderFromControl(streamIdInput);
  });
  fieldset.append(createFilterField("filter-stream-id", "Stream ID", streamIdInput));

  const wireVersionSelect = createElement("select");
  wireVersionSelect.append(createSelectOption("", "All wire versions"));
  for (const wireVersion of filterOptions.wireVersions) {
    wireVersionSelect.append(createSelectOption(String(wireVersion), `V${wireVersion}`));
  }
  wireVersionSelect.value = filters.wireVersion;
  wireVersionSelect.addEventListener("change", () => {
    filters.wireVersion = wireVersionSelect.value;
    rerenderFromControl(wireVersionSelect);
  });
  fieldset.append(createFilterField("filter-wire-version", "Wire version", wireVersionSelect));

  const occupancySelect = createElement("select");
  occupancySelect.append(createSelectOption("", "All occupancy"));
  for (const occupancy of filterOptions.occupancies) {
    const noun = occupancy === 1 ? "connection" : "connections";
    occupancySelect.append(createSelectOption(String(occupancy), `${occupancy} ${noun}`));
  }
  occupancySelect.value = filters.occupancy;
  occupancySelect.addEventListener("change", () => {
    filters.occupancy = occupancySelect.value;
    rerenderFromControl(occupancySelect);
  });
  fieldset.append(createFilterField("filter-occupancy", "PDC occupancy", occupancySelect));

  const activeScenarioSelect = createElement("select");
  activeScenarioSelect.append(createSelectOption("", "All active scenarios"));
  for (const scenarioName of filterOptions.activeScenarios) {
    activeScenarioSelect.append(createSelectOption(scenarioName, scenarioName));
  }
  activeScenarioSelect.value = filters.activeScenario;
  activeScenarioSelect.addEventListener("change", () => {
    filters.activeScenario = activeScenarioSelect.value;
    rerenderFromControl(activeScenarioSelect);
  });
  fieldset.append(createFilterField("filter-active-scenario", "Active scenario", activeScenarioSelect));

  const timeHealthSelect = createElement("select");
  timeHealthSelect.append(createSelectOption("", "All Time Health"));
  for (const timeHealth of filterOptions.timeHealth) {
    timeHealthSelect.append(createSelectOption(timeHealth, humanizeIdentifier(timeHealth)));
  }
  timeHealthSelect.value = filters.timeHealth;
  timeHealthSelect.addEventListener("change", () => {
    filters.timeHealth = timeHealthSelect.value;
    rerenderFromControl(timeHealthSelect);
  });
  fieldset.append(createFilterField("filter-time-health", "Time Health", timeHealthSelect));

  form.append(fieldset);
  appendChildren(section, [heading, form, table.tableScroll]);
  return section;
}

function buildSnapshotContent(snapshot, filters, onFiltersChanged, controls) {
  const fragment = document.createDocumentFragment();
  fragment.append(createCatalogSection(snapshot));
  fragment.append(createPmuSection(snapshot, filters, onFiltersChanged, controls));
  return fragment;
}

function reasonFor(error) {
  const kind = error instanceof ConsoleSnapshotError ? error.kind : "response";
  switch (kind) {
    case "network":
      return "The console service could not be reached.";
    case "payload":
      return "The console service returned an invalid console snapshot.";
    case "catalog_mismatch":
      return "The catalog changed during the console refresh.";
    case "stale_cursor":
    case "coherence":
      return "The console state changed during the console refresh.";
    default:
      return "The console service did not return a usable state.";
  }
}

function setFilterControlsEnabled(content, status, enabled, focusStatusBeforeDisabling) {
  const fieldsets = [...content.querySelectorAll(".filters fieldset")];
  if (
    !enabled
    && focusStatusBeforeDisabling
    && fieldsets.some((fieldset) => fieldset.contains(document.activeElement))
  ) {
    if (!status.hasAttribute("tabindex")) {
      status.setAttribute("tabindex", "-1");
    }
    status.focus();
  }
  for (const fieldset of fieldsets) {
    fieldset.disabled = !enabled;
  }
}

function bootstrap() {
  const content = document.getElementById("console-content");
  const connectionState = document.getElementById("connection-state");
  const lastSuccess = document.getElementById("last-success");
  const catalogIdentity = document.getElementById("catalog-identity");
  const status = document.getElementById("console-status");
  const operationStatus = document.getElementById("operation-status");
  const scenarioDialog = document.getElementById("scenario-dialog");
  const scenarioDialogContent = document.getElementById("scenario-dialog-content");
  const desktopViewport = window.matchMedia("(min-width: 1024px)");
  if (
    !content
    || !connectionState
    || !lastSuccess
    || !catalogIdentity
    || !status
    || !operationStatus
    || !scenarioDialog
    || !scenarioDialogContent
  ) {
    return;
  }

  const filters = {
    streamId: "",
    wireVersion: "",
    occupancy: "",
    activeScenario: "",
    timeHealth: "",
  };
  let completeSnapshot = null;
  let lastSuccessfulUpdate = null;
  let refreshPromise = null;
  let pollTimer = null;
  let connectionPresentation = "unavailable";
  let managementRequestInFlight = false;
  let dialogState = null;

  function managementControlsEnabled() {
    return completeSnapshot !== null
      && connectionPresentation === "online"
      && desktopViewport.matches
      && !managementRequestInFlight;
  }

  function setOperationStatus(kind, message) {
    operationStatus.hidden = message === "";
    operationStatus.textContent = message;
    operationStatus.className = `operation-status operation-status--${kind}`;
  }

  function clearOperationStatus() {
    setOperationStatus("info", "");
  }

  function updateConfirmationCountdowns() {
    const now = Date.now();
    for (const countdown of document.querySelectorAll("[data-confirm-countdown]")) {
      const expiresAt = Number(countdown.dataset.confirmExpiresAt);
      if (!Number.isFinite(expiresAt)) {
        continue;
      }
      const expired = expiresAt <= now;
      countdown.textContent = expired
        ? "Confirmation expired"
        : `Confirm within ${formatConfirmationCountdown(expiresAt, now)}`;
      countdown.classList.toggle("scenario-countdown--expired", expired);
    }
    for (const control of document.querySelectorAll("[data-management-control][data-confirm-expires-at]")) {
      const expiresAt = Number(control.dataset.confirmExpiresAt);
      control.disabled = !managementControlsEnabled()
        || control.dataset.targetActionAvailable === "false"
        || !Number.isFinite(expiresAt)
        || expiresAt <= now;
    }
  }

  function setManagementControlsEnabled() {
    const enabled = managementControlsEnabled();
    for (const control of document.querySelectorAll("[data-management-control]")) {
      if (!control.hasAttribute("data-confirm-expires-at")) {
        control.disabled = !enabled || control.dataset.targetActionAvailable === "false";
      }
    }
    updateConfirmationCountdowns();
  }

  window.setInterval(updateConfirmationCountdowns, CONFIRMATION_TICK_MS);

  function setConnectionPresentation(state, message) {
    connectionPresentation = state;
    const className = `connection-state--${state}`;
    connectionState.textContent = state === "online"
      ? "Online"
      : state === "stale"
        ? "Console Stale State"
        : "Unavailable";
    connectionState.className = `connection-state ${className}`;
    status.textContent = message;
    status.className = `console-status console-status--${state}`;
    setFilterControlsEnabled(content, status, state === "online", state === "stale");
    setManagementControlsEnabled();
  }

  function closeScenarioDialog() {
    dialogState = null;
    if (!scenarioDialog.open && !scenarioDialog.hasAttribute("open")) {
      return;
    }
    if (typeof scenarioDialog.close === "function") {
      scenarioDialog.close();
    } else {
      scenarioDialog.removeAttribute("open");
    }
  }

  function showScenarioDialog(nextDialogState) {
    if (!desktopViewport.matches) {
      closeScenarioDialog();
      setOperationStatus("info", "Scenario actions are available on desktop viewports.");
      return;
    }
    dialogState = nextDialogState;
    renderScenarioDialog();
  }

  desktopViewport.addEventListener("change", () => {
    if (!desktopViewport.matches) {
      closeScenarioDialog();
      setOperationStatus("info", "Scenario actions are available on desktop viewports.");
    }
    setManagementControlsEnabled();
  });

  function createScenarioDialogHeader(title, target, detail) {
    const header = createElement("header", { className: "scenario-dialog__header" });
    appendChildren(header, [
      createElement("p", { className: "eyebrow", text: "Scenario action" }),
      createElement("h2", { text: title, attributes: { id: "scenario-dialog-title" } }),
      createElement("p", { className: "scenario-dialog__target", text: targetDescription(target) }),
      createElement("p", { className: "scenario-dialog__detail", text: detail }),
    ]);
    return header;
  }

  function createOperatorForm(state, submitLabel, onSubmit) {
    const form = createElement("form", { className: "scenario-dialog__form" });
    const field = createElement("div", { className: "scenario-dialog__field" });
    const label = createElement("label", {
      text: "Operator label",
      attributes: { for: "scenario-operator-label" },
    });
    const input = createElement("input", {
      attributes: {
        id: "scenario-operator-label",
        type: "text",
        name: "operator_label",
        required: true,
        maxlength: String(MAX_OPERATOR_LABEL_UTF8_BYTES),
        autocomplete: "name",
        value: state.actorLabel ?? "",
        "data-management-control": "",
        disabled: !managementControlsEnabled(),
      },
    });
    input.addEventListener("input", () => {
      state.actorLabel = input.value;
      input.setCustomValidity("");
    });
    field.append(label, input);

    const actions = createElement("div", { className: "scenario-dialog__actions" });
    const closeButton = createElement("button", {
      className: "scenario-action",
      text: "Close",
      attributes: { type: "button" },
    });
    closeButton.addEventListener("click", closeScenarioDialog);
    const submitButton = createElement("button", {
      className: "scenario-action scenario-action--confirm",
      text: submitLabel,
      attributes: {
        type: "submit",
        "data-management-control": "",
        disabled: !managementControlsEnabled(),
      },
    });
    actions.append(closeButton, submitButton);
    form.append(field, actions);
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      const actorLabel = input.value.trim();
      if (actorLabel === "") {
        input.setCustomValidity("Enter an operator label.");
        input.reportValidity();
        return;
      }
      if (utf8ByteLength(actorLabel) > MAX_OPERATOR_LABEL_UTF8_BYTES) {
        input.setCustomValidity(
          `Operator label must be at most ${MAX_OPERATOR_LABEL_UTF8_BYTES} UTF-8 bytes.`,
        );
        input.reportValidity();
        return;
      }
      input.setCustomValidity("");
      state.actorLabel = actorLabel;
      onSubmit(actorLabel);
    });
    return { form, input };
  }

  function preparedRecord(token) {
    if (completeSnapshot === null) {
      return null;
    }
    return completeSnapshot.scenarioController.prepared.find((record) => record.token === token) ?? null;
  }

  function renderScenarioDialog() {
    if (dialogState === null) {
      return;
    }

    const fragment = document.createDocumentFragment();
    let form = null;
    let focusInput = null;
    if (dialogState.type === "prepare") {
      fragment.append(createScenarioDialogHeader(
        "Prepare scenario",
        dialogState.target,
        `Prepare ${dialogState.scenario.name}. Confirmation will be required before it runs.`,
      ));
      const operatorForm = createOperatorForm(dialogState, "Prepare scenario", (actorLabel) => {
        void submitPrepareScenario(dialogState.target, dialogState.scenario, actorLabel);
      });
      form = operatorForm.form;
      focusInput = operatorForm.input;
    } else if (dialogState.type === "clear") {
      fragment.append(createScenarioDialogHeader(
        "Prepare clear",
        dialogState.target,
        `Prepare a clear for sustained scenario ${dialogState.activeScenario.scenarioName}. Confirmation will be required.`,
      ));
      const operatorForm = createOperatorForm(dialogState, "Prepare clear", (actorLabel) => {
        void submitClearScenario(dialogState.target, actorLabel);
      });
      form = operatorForm.form;
      focusInput = operatorForm.input;
    } else {
      const record = preparedRecord(dialogState.record.token) ?? dialogState.record;
      dialogState.record = record;
      const expiresAt = confirmationExpiresAt(record);
      const expired = expiresAt <= Date.now();
      const isCancellation = dialogState.type === "cancel";
      fragment.append(createScenarioDialogHeader(
        isCancellation ? "Cancel prepared action" : "Confirm prepared action",
        record.target,
        isCancellation
          ? `Cancel ${actionDescription(record.action)} directly.`
          : `Confirm ${actionDescription(record.action)} before the deadline.`,
      ));
      fragment.append(createElement("output", {
        className: "scenario-dialog__countdown scenario-countdown",
        text: expired ? "Confirmation expired" : `Confirm within ${formatConfirmationCountdown(expiresAt)}`,
        attributes: {
          "data-confirm-countdown": "",
          "data-confirm-expires-at": expiresAt,
        },
      }));
      const operatorForm = createOperatorForm(
        dialogState,
        isCancellation ? "Cancel prepared action" : "Confirm",
        (actorLabel) => {
          if (isCancellation) {
            void submitCancellation(record, actorLabel);
          } else {
            void submitConfirmation(record, actorLabel);
          }
        },
      );
      const submitButton = operatorForm.form.querySelector("button[type=submit]");
      if (submitButton !== null) {
        submitButton.disabled = !managementControlsEnabled() || expired;
        submitButton.setAttribute("data-confirm-expires-at", String(expiresAt));
      }
      form = operatorForm.form;
      focusInput = operatorForm.input;
    }

    if (form !== null) {
      fragment.append(form);
    }
    scenarioDialogContent.replaceChildren(fragment);
    if (!scenarioDialog.open && !scenarioDialog.hasAttribute("open")) {
      if (typeof scenarioDialog.showModal === "function") {
        scenarioDialog.showModal();
      } else {
        scenarioDialog.setAttribute("open", "");
      }
    }
    setManagementControlsEnabled();
    if (focusInput !== null && !focusInput.disabled) {
      focusInput.focus();
    }
  }

  function closeUnavailableScenarioDialog(message, announceUnavailable) {
    closeScenarioDialog();
    if (announceUnavailable) {
      setOperationStatus("info", message);
    }
  }

  function reconcileScenarioDialog(options = {}) {
    if (dialogState === null || completeSnapshot === null) {
      return;
    }
    const announceUnavailable = options.announceUnavailable !== false;
    if (dialogState.type === "prepare") {
      const availability = targetScenarioAvailability(completeSnapshot, dialogState.target);
      const scenarioAvailable = scenariosForTarget(completeSnapshot, dialogState.target)
        .some((scenario) => scenario.name === dialogState.scenario.name);
      if (availability.runAvailable && scenarioAvailable) {
        return;
      }
      closeUnavailableScenarioDialog(
        "The target is no longer available to prepare this scenario.",
        announceUnavailable,
      );
      return;
    }

    if (dialogState.type === "clear") {
      const availability = targetScenarioAvailability(completeSnapshot, dialogState.target);
      const activeRecord = completeSnapshot.scenarioController.active.find((record) => {
        return activeScenarioFocusKey(record) === activeScenarioFocusKey(dialogState.activeScenario);
      });
      if (
        activeRecord !== undefined
        && activeRecord.lifecycle.toLowerCase() === "sustained"
        && availability.clearAvailable
      ) {
        dialogState.activeScenario = activeRecord;
        return;
      }
      closeUnavailableScenarioDialog(
        "The sustained scenario is no longer available to clear.",
        announceUnavailable,
      );
      return;
    }

    const currentRecord = preparedRecord(dialogState.record.token);
    if (currentRecord === null) {
      if (!managementRequestInFlight) {
        closeUnavailableScenarioDialog(
          "The prepared action is no longer available.",
          announceUnavailable,
        );
      }
      return;
    }
    dialogState.record = currentRecord;
    const expiresAt = confirmationExpiresAt(currentRecord);
    if (dialogState.type === "cancel" && expiresAt <= Date.now() && !managementRequestInFlight) {
      closeUnavailableScenarioDialog(
        "The prepared action has expired.",
        announceUnavailable,
      );
      return;
    }
    for (const element of scenarioDialog.querySelectorAll("[data-confirm-expires-at]")) {
      element.setAttribute("data-confirm-expires-at", String(expiresAt));
    }
    updateConfirmationCountdowns();
  }

  function actionMatches(left, right) {
    return left.kind === right.kind && left.scenarioName === right.scenarioName;
  }

  function findPreparedAction(target, action, actorLabel) {
    if (completeSnapshot === null) {
      return null;
    }
    const candidates = completeSnapshot.scenarioController.prepared.filter((record) => {
      return targetsMatch(record.target, target) && actionMatches(record.action, action);
    });
    return candidates.find((record) => record.actorLabel === actorLabel) ?? candidates[0] ?? null;
  }

  function beginManagementRequest() {
    if (!managementControlsEnabled()) {
      setOperationStatus("error", "Scenario management is unavailable until the console has a current snapshot.");
      return false;
    }
    managementRequestInFlight = true;
    clearOperationStatus();
    setManagementControlsEnabled();
    return true;
  }

  function finishManagementRequest() {
    managementRequestInFlight = false;
    setManagementControlsEnabled();
    reconcileScenarioDialog({ announceUnavailable: false });
  }

  function requestFailureMessage(error) {
    return error instanceof ScenarioRequestError
      ? error.message
      : "The scenario management request could not be completed.";
  }

  async function submitPreparedAction(endpoint, body, target, action, actorLabel, successMessage) {
    if (!beginManagementRequest()) {
      return;
    }
    let failureMessage = null;
    try {
      await postScenarioAction(endpoint, body);
    } catch (error) {
      failureMessage = requestFailureMessage(error);
    }

    try {
      const snapshot = await refresh({ fresh: true });
      if (failureMessage !== null) {
        setOperationStatus("error", failureMessage);
        return;
      }
      const record = snapshot === null ? null : findPreparedAction(target, action, actorLabel);
      if (record === null) {
        closeScenarioDialog();
        setOperationStatus(
          "success",
          snapshot === null
            ? `${successMessage} Awaiting a refreshed console state.`
            : `${successMessage} Console state was reconciled.`,
        );
      } else {
        setOperationStatus("success", `${successMessage} Confirmation is required.`);
        showScenarioDialog({ type: "confirm", record, actorLabel });
      }
    } finally {
      finishManagementRequest();
    }
  }

  async function submitPrepareScenario(target, scenario, actorLabel) {
    await submitPreparedAction(
      "prepare",
      {
        target: targetPayload(target),
        scenario_name: scenario.name,
        actor_label: actorLabel,
      },
      target,
      { kind: "activate", scenarioName: scenario.name },
      actorLabel,
      `${scenario.name} was prepared.`,
    );
  }

  async function submitClearScenario(target, actorLabel) {
    await submitPreparedAction(
      "clear",
      {
        target: targetPayload(target),
        actor_label: actorLabel,
      },
      target,
      { kind: "clear", scenarioName: null },
      actorLabel,
      "The clear action was prepared.",
    );
  }

  async function submitConfirmation(record, actorLabel) {
    if (!beginManagementRequest()) {
      return;
    }
    const token = record.token;
    let failureMessage = null;
    try {
      await postScenarioAction("confirm", { token, actor_label: actorLabel });
    } catch (error) {
      failureMessage = requestFailureMessage(error);
    }

    try {
      if (failureMessage === null) {
        closeScenarioDialog();
      }
      const snapshot = await refresh({ fresh: true });
      setOperationStatus(
        failureMessage === null ? "success" : "error",
        failureMessage === null
          ? snapshot === null
            ? "Confirmation accepted. Awaiting a refreshed console state."
            : "Confirmation accepted. Console state was reconciled."
          : failureMessage,
      );
    } finally {
      finishManagementRequest();
    }
  }

  async function submitCancellation(record, actorLabel) {
    const currentRecord = preparedRecord(record.token) ?? record;
    if (confirmationExpiresAt(currentRecord) <= Date.now()) {
      closeUnavailableScenarioDialog(
        "The prepared action has expired. Refreshing the console state.",
        true,
      );
      void refresh({ fresh: true });
      return;
    }
    if (!beginManagementRequest()) {
      return;
    }
    let failureMessage = null;
    try {
      await postScenarioAction("cancel", { token: currentRecord.token, actor_label: actorLabel });
    } catch (error) {
      failureMessage = requestFailureMessage(error);
    }

    try {
      if (failureMessage === null) {
        closeScenarioDialog();
      }
      const snapshot = await refresh({ fresh: true });
      setOperationStatus(
        failureMessage === null ? "success" : "error",
        failureMessage === null
          ? snapshot === null
            ? "Prepared action cancelled. Awaiting a refreshed console state."
            : "Prepared action cancelled. Console state was reconciled."
          : failureMessage,
      );
    } finally {
      finishManagementRequest();
    }
  }

  function scenarioControls() {
    return {
      enabled: managementControlsEnabled(),
      onRunScenario(target, scenario) {
        showScenarioDialog({ type: "prepare", target, scenario, actorLabel: "" });
      },
      onConfirmPrepared(record) {
        showScenarioDialog({ type: "confirm", record, actorLabel: "" });
      },
      onCancelPrepared(record) {
        showScenarioDialog({ type: "cancel", record, actorLabel: "" });
      },
      onClearScenario(target, activeScenario) {
        showScenarioDialog({ type: "clear", target, activeScenario, actorLabel: "" });
      },
    };
  }

  scenarioDialog.addEventListener("cancel", () => {
    dialogState = null;
  });
  scenarioDialog.addEventListener("close", () => {
    dialogState = null;
  });

  function updateHeader(snapshot) {
    lastSuccess.textContent = formatTimestamp(lastSuccessfulUpdate);
    catalogIdentity.textContent = `v${snapshot.catalog.version} / ${snapshot.catalog.contentSha256}`;
  }

  function openScenarioRunMenu() {
    if (typeof CSS === "undefined" || !CSS.supports("selector(select:open)")) {
      return null;
    }
    return [...content.querySelectorAll(".scenario-run-menu")].find((menu) => {
      return menu.matches(":open");
    }) ?? null;
  }

  function nodePath(root, node) {
    const path = [];
    let current = node;
    while (current !== root) {
      const parent = current.parentNode;
      if (parent === null) {
        return null;
      }
      const index = [...parent.childNodes].indexOf(current);
      if (index === -1) {
        return null;
      }
      path.unshift({ node: current, index });
      current = parent;
    }
    return path;
  }

  function matchingNodePaths(currentPath, replacementPath) {
    return currentPath !== null
      && replacementPath !== null
      && currentPath.length === replacementPath.length
      && currentPath.every((entry, pathIndex) => {
        const replacementEntry = replacementPath[pathIndex];
        return entry.index === replacementEntry.index
          && entry.node.nodeType === replacementEntry.node.nodeType
          && entry.node.nodeName === replacementEntry.node.nodeName;
      });
  }

  function preserveOpenScenarioRunMenu(replacement) {
    const currentMenu = openScenarioRunMenu();
    if (currentMenu === null) {
      return false;
    }
    const focusKey = currentMenu.getAttribute("data-focus-key");
    if (focusKey === null) {
      return false;
    }
    const replacementMenu = [...replacement.querySelectorAll(".scenario-run-menu")].find((candidate) => {
      return candidate.getAttribute("data-focus-key") === focusKey;
    }) ?? null;
    if (
      replacementMenu === null
      || replacementMenu.dataset.targetActionAvailable !== "true"
      || !currentMenu.isEqualNode(replacementMenu)
    ) {
      return false;
    }

    const currentPath = nodePath(content, currentMenu);
    const replacementPath = nodePath(replacement, replacementMenu);
    if (!matchingNodePaths(currentPath, replacementPath)) {
      return false;
    }

    for (let pathIndex = 0; pathIndex < currentPath.length; pathIndex += 1) {
      const currentEntry = currentPath[pathIndex];
      const replacementEntry = replacementPath[pathIndex];
      const currentParent = pathIndex === 0 ? content : currentPath[pathIndex - 1].node;
      const replacementParent = pathIndex === 0
        ? replacement
        : replacementPath[pathIndex - 1].node;
      const replacementChildren = [...replacementParent.childNodes];
      const replacementChildIndex = replacementChildren.indexOf(replacementEntry.node);

      for (const child of [...currentParent.childNodes]) {
        if (child !== currentEntry.node) {
          child.remove();
        }
      }
      for (const child of replacementChildren.slice(0, replacementChildIndex)) {
        currentParent.insertBefore(child, currentEntry.node);
      }
      for (const child of replacementChildren.slice(replacementChildIndex + 1)) {
        currentParent.append(child);
      }
    }
    return true;
  }

  function captureContentFocus() {
    const activeElement = document.activeElement;
    if (
      activeElement === null
      || !content.contains(activeElement)
    ) {
      return null;
    }
    const focusKey = activeElement.getAttribute("data-focus-key");
    if (focusKey === null && !activeElement.id.startsWith("filter-")) {
      return null;
    }
    const focusState = focusKey === null ? { id: activeElement.id } : { focusKey };
    if (
      typeof activeElement.selectionStart === "number"
      && typeof activeElement.selectionEnd === "number"
    ) {
      focusState.selectionStart = activeElement.selectionStart;
      focusState.selectionEnd = activeElement.selectionEnd;
    }
    return focusState;
  }

  function capturePdcDetailsState() {
    const openKeys = new Set();
    for (const details of content.querySelectorAll("details[data-pdc-details-key]")) {
      const key = details.dataset.pdcDetailsKey;
      if (key === undefined) {
        continue;
      }
      if (details.open) {
        openKeys.add(key);
      }
    }
    return { openKeys };
  }

  function restorePdcDetailsState(state) {
    for (const details of content.querySelectorAll("details[data-pdc-details-key]")) {
      const key = details.dataset.pdcDetailsKey;
      if (key === undefined) {
        continue;
      }
      if (state.openKeys.has(key)) {
        details.open = true;
      }
    }
  }

  function renderCompleteSnapshot(focusState = captureContentFocus()) {
    const pdcDetailsState = capturePdcDetailsState();
    const replacement = buildSnapshotContent(
      completeSnapshot,
      filters,
      renderCompleteSnapshot,
      scenarioControls(),
    );
    if (!preserveOpenScenarioRunMenu(replacement)) {
      content.replaceChildren(replacement);
    }
    restorePdcDetailsState(pdcDetailsState);
    setManagementControlsEnabled();
    reconcileScenarioDialog();
    if (focusState === null) {
      return;
    }
    const control = focusState.focusKey === undefined
      ? document.getElementById(focusState.id)
      : [...content.querySelectorAll("[data-focus-key]")].find((candidate) => {
        return candidate.getAttribute("data-focus-key") === focusState.focusKey;
      }) ?? null;
    if (control === null || !content.contains(control)) {
      return;
    }
    if (control !== openScenarioRunMenu()) {
      control.focus();
    }
    if (
      typeof focusState.selectionStart === "number"
      && typeof focusState.selectionEnd === "number"
      && typeof control.setSelectionRange === "function"
    ) {
      control.setSelectionRange(focusState.selectionStart, focusState.selectionEnd);
    }
  }

  function scheduleNextRefresh() {
    if (pollTimer !== null) {
      window.clearTimeout(pollTimer);
    }
    pollTimer = window.setTimeout(refresh, POLL_INTERVAL_MS);
  }

  function refresh(options = {}) {
    const fresh = options.fresh === true;
    if (refreshPromise !== null) {
      if (!fresh) {
        return refreshPromise;
      }
      const inFlightRefresh = refreshPromise;
      return inFlightRefresh.then(() => refresh());
    }
    refreshPromise = (async () => {
      try {
        const snapshot = await loadCoherentSnapshot();
        const receivedAt = Date.now();
        for (const record of snapshot.scenarioController.prepared) {
          record.confirmExpiresAtMs = receivedAt + record.confirmExpiresInMs;
        }
        completeSnapshot = snapshot;
        lastSuccessfulUpdate = new Date(receivedAt);
        renderCompleteSnapshot();
        updateHeader(snapshot);
        setConnectionPresentation("online", "Online. Coherent console snapshot updated.");
        return snapshot;
      } catch (error) {
        const reason = reasonFor(error);
        if (completeSnapshot === null) {
          setConnectionPresentation("unavailable", `Unavailable. ${reason}`);
        } else {
          setConnectionPresentation("stale", `Console Stale State. Retaining the last coherent snapshot. ${reason}`);
        }
        return null;
      } finally {
        refreshPromise = null;
        scheduleNextRefresh();
      }
    })();
    return refreshPromise;
  }

  refresh();
}

if (typeof window !== "undefined" && typeof document !== "undefined") {
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", bootstrap, { once: true });
  } else {
    bootstrap();
  }
}