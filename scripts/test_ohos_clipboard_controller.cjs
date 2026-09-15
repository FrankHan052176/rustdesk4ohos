#!/usr/bin/env node
'use strict';

/**
 * Behavior regressions for the Flutter OHOS clipboard bridge.
 *
 * Two production ArkTS modules
 * (flutter/ohos/entry/src/main/ets/platform/RustDeskClipboardController.ets
 * and RustDeskPlatformBridge.ets) are read as text, transpiled with the
 * HarmonyOS SDK's TypeScript and executed inside node:vm. The controller
 * regressions drive RustDeskClipboardController directly; the bridge
 * regressions drive the real MethodChannel handler (onAttachedToAbility,
 * onDetachedFromAbility, onMethodCall) with fake MethodCall/MethodResult
 * objects, so the bridge loads the controller through its own relative import.
 *
 * Only the SDK modules the loaded production modules import are faked:
 *
 *   @kit.AbilityKit         abilityAccessCtrl (permission gate), common, Permissions, wantAgent
 *   @kit.AudioKit           audio (never reached by these regressions)
 *   @kit.BasicServicesKit   pasteboard (SystemPasteboard + PasteData + records), deviceInfo
 *   @kit.BackgroundTasksKit backgroundTaskManager (never reached)
 *   @kit.LocalizationKit    i18n (never reached)
 *   @kit.ArkUI              display, window (never reached)
 *   @kit.CoreFileKit        fileIo (in-memory tree), fileUri, picker (never reached)
 *   @kit.ImageKit           image (ImageSource / PixelMap / ImagePacker)
 *   @ohos/flutter_ohos      MethodChannel (constructed only by onAttachedToEngine)
 *
 * Production logic is never rewritten, replaced or source-text asserted: every
 * assertion drives real controller/bridge methods and observes the fake SDK
 * boundary. All waits are deterministic (microtask/macrotask drains and
 * predicate polls); there are no sleeps and no temporary files.
 *
 * Run:
 *   NODE_PATH=/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/ets/build-tools/ets-loader/node_modules \
 *     node --test scripts/test_ohos_clipboard_controller.cjs
 *
 * Both NODE_PATH and OHOS_ETS_TYPESCRIPT_DIR point at the directory that
 * contains the `typescript` package (the SDK ships it as `ohos-typescript`).
 */

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const CONTROLLER_PATH = path.resolve(
  __dirname,
  '..',
  'flutter',
  'ohos',
  'entry',
  'src',
  'main',
  'ets',
  'platform',
  'RustDeskClipboardController.ets',
);

const BRIDGE_PATH = path.resolve(
  __dirname,
  '..',
  'flutter',
  'ohos',
  'entry',
  'src',
  'main',
  'ets',
  'platform',
  'RustDeskPlatformBridge.ets',
);

const TEST_TIMEOUT = 15000;

const DEFAULT_FILES_DIR = '/data/accounts/100/app/el2/base/haps/entry/files';
const DEFAULT_CACHE_DIR = '/data/accounts/100/app/el2/base/haps/entry/cache';
const DEFAULT_ACCESS_TOKEN_ID = 4242;

const MIME = {
  TEXT_PLAIN: 'text/plain',
  TEXT_HTML: 'text/html',
  TEXT_URI: 'text/uri',
  PIXELMAP: 'pixelMap',
};

/* ------------------------------------------------------------------ *
 * TypeScript + module loading
 * ------------------------------------------------------------------ */

const TYPESCRIPT_SEARCH_PATHS = [
  process.env.OHOS_ETS_TYPESCRIPT_DIR,
  '/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/ets/build-tools/ets-loader/node_modules/typescript',
].filter((candidate) => typeof candidate === 'string' && candidate.length > 0);

function loadTypeScript() {
  try {
    return require('typescript');
  } catch (_) {
    // Fall through to the SDK copy below.
  }
  for (const candidate of TYPESCRIPT_SEARCH_PATHS) {
    if (fs.existsSync(path.join(candidate, 'package.json'))) {
      return require(candidate);
    }
  }
  throw new Error(
    'typescript is unavailable: set NODE_PATH to the directory containing the typescript package '
      + `or OHOS_ETS_TYPESCRIPT_DIR to the package directory (tried: ${TYPESCRIPT_SEARCH_PATHS.join(', ')})`,
  );
}

const ts = loadTypeScript();

if (!fs.existsSync(CONTROLLER_PATH)) {
  throw new Error(`production clipboard controller not found at ${CONTROLLER_PATH}`);
}

if (!fs.existsSync(BRIDGE_PATH)) {
  throw new Error(`production platform bridge not found at ${BRIDGE_PATH}`);
}

const transpileCache = new Map();

function transpileSource(source, fileName) {
  const cached = transpileCache.get(fileName);
  if (cached !== undefined) {
    return cached;
  }
  const output = ts.transpileModule(source, {
    fileName: `${fileName}.ts`,
    compilerOptions: {
      target: ts.ScriptTarget.ES2021,
      module: ts.ModuleKind.CommonJS,
      esModuleInterop: true,
    },
    reportDiagnostics: false,
  }).outputText;
  transpileCache.set(fileName, output);
  return output;
}

/**
 * Loads the production entry file (and any local `.ets`/`.ts` module it
 * imports) as CommonJS inside the given vm sandbox. SDK specifiers resolve to
 * the fakes; relative specifiers are read from disk and transpiled too.
 */
function evaluateProductionModule(sandbox, fakeModules, entryPath) {
  const context = vm.createContext(sandbox);
  const loaded = new Map();

  function resolveLocal(specifier, fromFile) {
    const base = path.resolve(path.dirname(fromFile), specifier);
    const candidates = [
      base,
      `${base}.ets`,
      `${base}.ts`,
      `${base}.js`,
      path.join(base, 'index.ets'),
      path.join(base, 'index.ts'),
    ];
    for (const candidate of candidates) {
      if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) {
        return candidate;
      }
    }
    throw new Error(`cannot resolve local import '${specifier}' from ${fromFile}`);
  }

  function load(filePath) {
    const resolved = path.resolve(filePath);
    const existing = loaded.get(resolved);
    if (existing !== undefined) {
      return existing.exports;
    }
    const moduleObject = { exports: {} };
    loaded.set(resolved, moduleObject);
    const code = transpileSource(fs.readFileSync(resolved, 'utf8'), resolved);
    const wrapper = vm.runInContext(
      `(function (exports, require, module, __filename, __dirname) {\n${code}\n})`,
      context,
      { filename: `${resolved}.js` },
    );
    const requireShim = (specifier) => {
      if (specifier.startsWith('.')) {
        return load(resolveLocal(specifier, resolved));
      }
      const fake = fakeModules[specifier];
      if (fake === undefined) {
        throw new Error(`unstubbed SDK import '${specifier}' required by ${resolved}`);
      }
      return fake;
    };
    wrapper(moduleObject.exports, requireShim, moduleObject, resolved, path.dirname(resolved));
    return moduleObject.exports;
  }

  return load(entryPath);
}

/* ------------------------------------------------------------------ *
 * Small async/deterministic helpers
 * ------------------------------------------------------------------ */

function deferred() {
  const gate = { settled: false, value: undefined, reason: undefined };
  gate.promise = new Promise((resolve, reject) => {
    gate.resolve = (value) => {
      if (gate.settled) {
        return;
      }
      gate.settled = true;
      gate.value = value;
      resolve(value);
    };
    gate.reject = (error) => {
      if (gate.settled) {
        return;
      }
      gate.settled = true;
      gate.reason = error;
      reject(error);
    };
  });
  return gate;
}

function turn() {
  return new Promise((resolve) => setImmediate(resolve));
}

async function drain(turns = 4) {
  for (let index = 0; index < turns; index += 1) {
    await turn();
  }
}

async function waitUntil(predicate, label, turns = 128) {
  for (let index = 0; index < turns; index += 1) {
    if (predicate()) {
      return;
    }
    await turn();
  }
  assert.fail(`timed out waiting for ${label}`);
}

function isThenable(value) {
  return value !== null
    && (typeof value === 'object' || typeof value === 'function')
    && typeof value.then === 'function';
}

function describe(value) {
  if (value === undefined) {
    return 'undefined';
  }
  if (value === null) {
    return 'null';
  }
  if (typeof value === 'string') {
    return JSON.stringify(value);
  }
  if (typeof value === 'object' || typeof value === 'function') {
    try {
      return JSON.stringify(value);
    } catch (_) {
      return Object.prototype.toString.call(value);
    }
  }
  return String(value);
}

function invoke(fn) {
  try {
    return { value: fn(), syncError: undefined };
  } catch (error) {
    return { value: undefined, syncError: error };
  }
}

function settle(promise) {
  return promise.then(
    (value) => ({ status: 'fulfilled', value }),
    (reason) => ({ status: 'rejected', reason }),
  );
}

/** Awaits a call that must signal failure, accepting a sync throw as failure. */
async function expectRejection(fn, label) {
  const call = invoke(fn);
  if (call.syncError !== undefined) {
    return call.syncError;
  }
  assert.ok(isThenable(call.value), `${label}: expected a promise, received ${describe(call.value)}`);
  const outcome = await settle(call.value);
  if (outcome.status === 'fulfilled') {
    assert.fail(`${label}: expected a rejection, resolved with ${describe(outcome.value)}`);
  }
  return outcome.reason;
}

async function settleCall(fn) {
  const call = invoke(fn);
  if (call.syncError !== undefined) {
    return { status: 'rejected', reason: call.syncError };
  }
  if (!isThenable(call.value)) {
    return { status: 'fulfilled', value: call.value };
  }
  return settle(call.value);
}

/* ------------------------------------------------------------------ *
 * Fake pasteboard
 * ------------------------------------------------------------------ */

class FakeRecord {
  static clone(record) {
    const copy = Object.create(FakeRecord.prototype);
    copy.mimeType = record.mimeType;
    copy.values = new Map(record.values);
    return copy;
  }

  constructor(mimeType, value) {
    this.mimeType = mimeType;
    this.values = new Map();
    if (value !== undefined) {
      this.values.set(mimeType, value);
    }
  }

  addEntry(mimeType, value) {
    this.values.set(mimeType, value);
  }

  get uri() {
    const value = this.values.get(MIME.TEXT_URI);
    return value === undefined ? '' : String(value);
  }

  get plainText() {
    const value = this.values.get(MIME.TEXT_PLAIN);
    return value === undefined ? '' : String(value);
  }

  get htmlText() {
    const value = this.values.get(MIME.TEXT_HTML);
    return value === undefined ? '' : String(value);
  }

  get pixelMap() {
    return this.values.get(MIME.PIXELMAP);
  }

  toPlainText() {
    if (this.plainText.length > 0) {
      return this.plainText;
    }
    const html = this.htmlText;
    if (html.length > 0) {
      return html.replace(/<[^>]*>/g, '');
    }
    return '';
  }
}

class FakePasteData {
  constructor(mimeType, value) {
    this.records = [];
    this.property = { shareOption: 0 };
    if (mimeType !== undefined) {
      this.records.push(new FakeRecord(mimeType, value));
    }
  }

  getRecordCount() {
    return this.records.length;
  }

  getRecord(index) {
    return this.records[index];
  }

  addRecord(mimeType, value) {
    this.records.push(new FakeRecord(mimeType, value));
  }

  getProperty() {
    return this.property;
  }

  setProperty(property) {
    this.property = property;
  }
}

function plainTextOf(records) {
  return records
    .filter((record) => record.mimeType === MIME.TEXT_PLAIN)
    .map((record) => record.plainText)
    .join('');
}

function urisOf(records) {
  const uris = [];
  for (const record of records) {
    const value = record.values.get(MIME.TEXT_URI);
    if (value !== undefined) {
      uris.push(String(value));
    }
  }
  return uris;
}

/**
 * Fake SystemPasteboard. Writes stay pending until the test commits or
 * rejects them, so the controller's ACK, serialization and changeCount
 * handling are observed instead of assumed.
 */
function createPasteboardKit() {
  const state = {
    records: [],
    changeCount: 0,
    listeners: new Set(),
    writes: [],
    commits: [],
    onFailure: null,
    onCalls: 0,
    offCalls: 0,
    getDataCalls: 0,
    hasDataCalls: 0,
  };

  function emitUpdate() {
    for (const listener of Array.from(state.listeners)) {
      listener();
    }
  }

  function firstUnsettled() {
    return state.writes.find((write) => !write.settled);
  }

  const system = {
    on(type, callback) {
      state.onCalls += 1;
      if (state.onFailure !== null) {
        const failure = state.onFailure;
        state.onFailure = null;
        throw failure;
      }
      state.listeners.add(callback);
    },
    off(type, callback) {
      state.offCalls += 1;
      state.listeners.delete(callback);
    },
    hasData() {
      state.hasDataCalls += 1;
      return Promise.resolve(state.records.length > 0);
    },
    hasDataType(mimeType) {
      return state.records.some((record) => record.values.has(mimeType));
    },
    getData() {
      state.getDataCalls += 1;
      const data = new FakePasteData();
      data.records = state.records.map(FakeRecord.clone);
      return Promise.resolve(data);
    },
    getChangeCount() {
      return state.changeCount;
    },
    setData(data) {
      const write = { data, settled: false };
      state.writes.push(write);
      return new Promise((resolve, reject) => {
        write.resolve = resolve;
        write.reject = reject;
      });
    },
  };

  const module = {
    getSystemPasteboard: () => system,
    MIMETYPE_TEXT_PLAIN: MIME.TEXT_PLAIN,
    MIMETYPE_TEXT_HTML: MIME.TEXT_HTML,
    MIMETYPE_TEXT_URI: MIME.TEXT_URI,
    MIMETYPE_PIXELMAP: MIME.PIXELMAP,
    ShareOption: { LOCALDEVICE: 0, CROSSDEVICE: 1 },
    createData: (mimeType, value) => new FakePasteData(mimeType, value),
    createPlainTextData: (text) => new FakePasteData(MIME.TEXT_PLAIN, text),
    createHtmlData: (html) => new FakePasteData(MIME.TEXT_HTML, html),
    createUriData: (uri) => new FakePasteData(MIME.TEXT_URI, uri),
  };

  const controls = {
    module,
    state,
    system,
    listenerCount: () => state.listeners.size,
    dispatchCount: () => state.writes.length,
    pendingWrites: () => state.writes.filter((write) => !write.settled).length,
    readCount: () => state.getDataCalls,
    commitCount: () => state.commits.length,
    changeCount: () => state.changeCount,
    currentText: () => plainTextOf(state.records),
    currentUris: () => urisOf(state.records),
    committedTexts: () => state.commits.map((commit) => commit.text),
    failNextListenerRegistration(error) {
      state.onFailure = error;
    },
    /** Completes the oldest pending setData, as the platform would. */
    commitWrite({ delta = 1 } = {}) {
      const write = firstUnsettled();
      if (write === undefined) {
        return false;
      }
      write.settled = true;
      state.records = write.data.records.map(FakeRecord.clone);
      state.changeCount += delta;
      state.commits.push({ text: plainTextOf(state.records), uris: urisOf(state.records) });
      emitUpdate();
      write.resolve();
      return true;
    },
    rejectWrite(error) {
      const write = firstUnsettled();
      if (write === undefined) {
        return false;
      }
      write.settled = true;
      write.reject(error);
      return true;
    },
    /** A write performed by another process or by the user. */
    foreignPublish(entries, { delta = 1 } = {}) {
      state.records = entries.map((entry) => new FakeRecord(entry.mimeType, entry.value));
      state.changeCount += delta;
      emitUpdate();
    },
    foreignText(text, options) {
      controls.foreignPublish([{ mimeType: MIME.TEXT_PLAIN, value: text }], options);
    },
    foreignUris(uris, options) {
      controls.foreignPublish(uris.map((uri) => ({ mimeType: MIME.TEXT_URI, value: uri })), options);
    },
  };

  return controls;
}

/* ------------------------------------------------------------------ *
 * Fake fileIo / fileUri
 * ------------------------------------------------------------------ */

function createVirtualFileSystem() {
  const nodes = new Map();

  function addDirectory(directory, { mtime = 0 } = {}) {
    nodes.set(directory, { type: 'directory', mtime });
  }

  function addFile(file, { mtime = 0, size = 0 } = {}) {
    nodes.set(file, { type: 'file', mtime, size });
  }

  function toPath(value) {
    return value.startsWith('file://') ? value.slice('file://'.length) : value;
  }

  function childrenOf(directory) {
    const prefix = directory.endsWith('/') ? directory : `${directory}/`;
    const names = new Set();
    for (const key of nodes.keys()) {
      if (key.startsWith(prefix)) {
        names.add(key.slice(prefix.length).split('/')[0]);
      }
    }
    return Array.from(names);
  }

  const fileIo = {
    listFileSync(directory) {
      if (nodes.get(directory)?.type !== 'directory') {
        throw new Error(`ENOENT: ${directory}`);
      }
      return childrenOf(directory);
    },
    lstatSync(target) {
      const node = nodes.get(target);
      if (node === undefined) {
        throw new Error(`ENOENT: ${target}`);
      }
      return {
        mtime: node.mtime,
        size: node.size ?? 0,
        isDirectory: () => node.type === 'directory',
        isFile: () => node.type === 'file',
        isSymbolicLink: () => node.type === 'symbolic-link',
      };
    },
    statSync(target) {
      const node = nodes.get(target);
      if (node === undefined || node.type !== 'file') {
        throw new Error(`ENOENT: ${target}`);
      }
      return { size: node.size ?? 0, isDirectory: () => false, isFile: () => true };
    },
    mkdirSync(target, recursive) {
      if (!recursive && nodes.has(target)) {
        throw new Error(`EEXIST: ${target}`);
      }
      nodes.set(target, { type: 'directory', mtime: 0 });
    },
    unlinkSync(target) {
      nodes.delete(target);
    },
    rmdirSync(target) {
      nodes.delete(target);
    },
    renameSync(from, to) {
      const node = nodes.get(from);
      nodes.delete(from);
      nodes.set(to, node);
    },
    copy(source, destination, options) {
      const sourcePath = toPath(source);
      const node = nodes.get(sourcePath);
      if (node === undefined) {
        throw new Error(`ENOENT: ${sourcePath}`);
      }
      const destinationPath = toPath(destination);
      const target = nodes.get(destinationPath)?.type === 'directory'
        ? `${destinationPath}/${sourcePath.split('/').pop()}`
        : destinationPath;
      nodes.set(target, { type: 'file', mtime: 0, size: node.size ?? 0 });
      if (options !== undefined && typeof options.progressListener === 'function') {
        options.progressListener({ processedSize: node.size ?? 0, totalSize: node.size ?? 0 });
      }
      return Promise.resolve();
    },
  };
  fileIo.TaskSignal = class TaskSignal {
    constructor() {
      this.cancelled = false;
    }

    cancel() {
      this.cancelled = true;
    }
  };

  const fileUri = {
    getUriFromPath: (target) => `file://${target}`,
  };

  return {
    fileIo,
    fileUri,
    nodes,
    addDirectory,
    addFile,
    has: (target) => nodes.has(target),
    paths: () => Array.from(nodes.keys()),
  };
}

/* ------------------------------------------------------------------ *
 * Fake ImageKit
 * ------------------------------------------------------------------ */

function createImageKit() {
  const state = { sources: [], rawDecodes: [], packers: [], releasedPixelMaps: 0 };

  function pixelMap() {
    return {
      release: () => {
        state.releasedPixelMaps += 1;
        return Promise.resolve();
      },
    };
  }

  const module = {
    PixelMapFormat: { RGB_565: 2, RGBA_8888: 3, BGRA_8888: 4 },
    AlphaType: { UNKNOWN: 0, OPAQUE: 1, UNPREMUL: 2, PREMUL: 3 },
    createImageSource(buffer) {
      const source = { buffer, imageInfo: deferred(), pixelMap: deferred(), released: false };
      source.getImageInfo = (index) => {
        source.imageInfoIndex = index;
        return source.imageInfo.promise;
      };
      source.createPixelMap = () => source.pixelMap.promise;
      source.release = () => {
        source.released = true;
        return Promise.resolve();
      };
      state.sources.push(source);
      return source;
    },
    createPixelMap(buffer, options) {
      const decode = { buffer, options, gate: deferred() };
      state.rawDecodes.push(decode);
      return decode.gate.promise;
    },
    createImagePacker() {
      const packer = {
        packToData: () => Promise.resolve(new ArrayBuffer(0)),
        release: () => Promise.resolve(),
      };
      state.packers.push(packer);
      return packer;
    },
  };

  const controls = {
    module,
    state,
    decodeStarted: () => state.sources.length + state.rawDecodes.length,
    /** Resolves whichever decode gate the controller is waiting on. */
    async resolveDecode({ width = 4, height = 4 } = {}) {
      for (let round = 0; round < 6; round += 1) {
        let progressed = false;
        for (const source of state.sources) {
          if (!source.imageInfo.settled) {
            source.imageInfo.resolve({ size: { width, height } });
            progressed = true;
          }
        }
        await turn();
        for (const source of state.sources) {
          if (source.imageInfo.settled && !source.pixelMap.settled) {
            source.pixelMap.resolve(pixelMap());
            progressed = true;
          }
        }
        for (const decode of state.rawDecodes) {
          if (!decode.gate.settled) {
            decode.gate.resolve(pixelMap());
            progressed = true;
          }
        }
        await turn();
        if (!progressed) {
          return;
        }
      }
    },
  };

  return controls;
}

/* ------------------------------------------------------------------ *
 * Fake Flutter OHOS plugin boundary
 * ------------------------------------------------------------------ */

/**
 * Minimal stand-in for `@ohos/flutter_ohos`. The bridge only constructs a
 * MethodChannel in onAttachedToEngine, which these regressions do not drive
 * (so host-to-Dart sends resolve false through the bridge's null-channel
 * guard); a Dart round trip that is attempted anyway is answered as
 * notImplemented rather than silently succeeding.
 */
function createFlutterOhosPlugin() {
  class MethodChannel {
    constructor(messenger, name) {
      this.messenger = messenger;
      this.name = name;
      this.handler = null;
      this.invocations = [];
    }

    setMethodCallHandler(handler) {
      this.handler = handler;
    }

    invokeMethod(method, args, result) {
      this.invocations.push({ method, args });
      result.notImplemented();
    }
  }

  return { MethodChannel };
}

/* ------------------------------------------------------------------ *
 * Harness
 * ------------------------------------------------------------------ */

/**
 * Builds the fake SDK boundary shared by the controller and bridge harnesses.
 * Every specifier imported by a loaded production module is present; the kits
 * these regressions never reach (audio, background tasks, i18n, display,
 * window, picker, deviceInfo, wantAgent) stay inert stubs, so a test that
 * wanders into them fails loudly instead of borrowing a richer fake.
 */
function createSdkFakes() {
  const pasteboard = createPasteboardKit();
  const files = createVirtualFileSystem();
  const image = createImageKit();
  const abilityAccessCtrl = {
    GrantStatus: { PERMISSION_GRANTED: 0, PERMISSION_DENIED: -1 },
    createAtManager: () => ({
      checkAccessToken: (tokenId, permission) =>
        Promise.resolve(abilityAccessCtrl.GrantStatus.PERMISSION_GRANTED),
      checkAccessTokenSync: (tokenId, permission) =>
        abilityAccessCtrl.GrantStatus.PERMISSION_GRANTED,
    }),
  };
  return {
    pasteboard,
    files,
    image,
    modules: {
      '@kit.AbilityKit': { abilityAccessCtrl, common: {}, Permissions: {}, wantAgent: {} },
      '@kit.AudioKit': { audio: {} },
      '@kit.BasicServicesKit': { pasteboard: pasteboard.module, deviceInfo: {} },
      '@kit.BackgroundTasksKit': { backgroundTaskManager: {} },
      '@kit.LocalizationKit': { i18n: {} },
      '@kit.ArkUI': { display: {}, window: {} },
      '@kit.CoreFileKit': { fileIo: files.fileIo, fileUri: files.fileUri, picker: {} },
      '@kit.ImageKit': { image: image.module },
      '@ohos/flutter_ohos': createFlutterOhosPlugin(),
    },
  };
}

function createSandbox() {
  return {
    console: { info() {}, warn() {}, error() {}, log() {} },
    setTimeout,
    clearTimeout,
    queueMicrotask,
  };
}

function createCallbackRecorder() {
  const calls = [];
  const callbacks = {
    sendText(sessionId, content) {
      calls.push({ kind: 'sendText', sessionId, content });
      return Promise.resolve(true);
    },
    sendImage(sessionId, bytes) {
      calls.push({ kind: 'sendImage', sessionId, bytes });
      return Promise.resolve(true);
    },
    sendFiles(sessionId, paths) {
      calls.push({ kind: 'sendFiles', sessionId, paths: Array.from(paths) });
      return Promise.resolve(true);
    },
    log(label, payload) {
      calls.push({ kind: 'log', label, payload });
    },
  };
  return {
    callbacks,
    calls,
    texts: () => calls.filter((call) => call.kind === 'sendText').map((call) => call.content),
    logs: () => calls.filter((call) => call.kind === 'log'),
    reset() {
      calls.length = 0;
    },
  };
}

function createHarness({
  filesDir = DEFAULT_FILES_DIR,
  cacheDir = DEFAULT_CACHE_DIR,
  accessTokenId = DEFAULT_ACCESS_TOKEN_ID,
} = {}) {
  const sdk = createSdkFakes();
  const productionModule = evaluateProductionModule(createSandbox(), sdk.modules, CONTROLLER_PATH);
  const Controller = productionModule.RustDeskClipboardController;
  assert.equal(
    typeof Controller,
    'function',
    `the production module must export RustDeskClipboardController (got: ${Object.keys(productionModule).join(', ')})`,
  );
  const recorder = createCallbackRecorder();
  const context = { filesDir, cacheDir, applicationInfo: { accessTokenId } };
  return {
    Controller,
    context,
    controller: new Controller(context, recorder.callbacks),
    callbacks: recorder.callbacks,
    calls: recorder.calls,
    texts: recorder.texts,
    logs: recorder.logs,
    resetCalls: recorder.reset,
    pasteboard: sdk.pasteboard,
    files: sdk.files,
    image: sdk.image,
    filesDir,
    cacheDir,
  };
}

function poisonContext(context) {
  const detached = () => {
    throw new Error('the ability context was detached');
  };
  for (const property of ['filesDir', 'cacheDir', 'applicationInfo']) {
    Object.defineProperty(context, property, { get: detached, configurable: true });
  }
}

/* ------------------------------------------------------------------ *
 * Bridge harness (MethodChannel boundary)
 * ------------------------------------------------------------------ */

function createMethodCall(method, args) {
  return {
    method,
    argument(name) {
      return Object.prototype.hasOwnProperty.call(args, name) ? args[name] : undefined;
    },
  };
}

/**
 * Records how the bridge answered one MethodChannel call, so an
 * unacknowledged Dart call is observable instead of assumed.
 */
function createMethodResult() {
  const calls = [];
  return {
    calls,
    get settled() {
      return calls.length > 0;
    },
    success(value) {
      calls.push({ kind: 'success', value });
    },
    error(code, message, data) {
      calls.push({ kind: 'error', code, message, data });
    },
    notImplemented() {
      calls.push({ kind: 'notImplemented' });
    },
    successes: () => calls.filter((call) => call.kind === 'success').map((call) => call.value),
    errors: () => calls.filter((call) => call.kind === 'error'),
  };
}

/**
 * Harness for the production MethodChannel handler. The bridge is built
 * without an engine, so every call enters the real onMethodCall switch; the
 * clipboard controller behind it is the production controller, loaded through
 * the bridge's own relative import.
 */
function createBridgeHarness({
  filesDir = DEFAULT_FILES_DIR,
  cacheDir = DEFAULT_CACHE_DIR,
  accessTokenId = DEFAULT_ACCESS_TOKEN_ID,
} = {}) {
  const sdk = createSdkFakes();
  const productionModule = evaluateProductionModule(createSandbox(), sdk.modules, BRIDGE_PATH);
  const Bridge = productionModule.default;
  assert.equal(
    typeof Bridge,
    'function',
    `the production module must default-export RustDeskPlatformBridge (got: ${Object.keys(productionModule).join(', ')})`,
  );
  const bridge = new Bridge();
  const context = { filesDir, cacheDir, applicationInfo: { accessTokenId } };
  const abilityBinding = { getAbility: () => ({ context }) };
  return {
    bridge,
    context,
    filesDir,
    cacheDir,
    pasteboard: sdk.pasteboard,
    files: sdk.files,
    image: sdk.image,
    attach: () => bridge.onAttachedToAbility(abilityBinding),
    detach: () => bridge.onDetachedFromAbility(),
    call(method, args = {}) {
      const result = createMethodResult();
      bridge.onMethodCall(createMethodCall(method, args), result);
      return result;
    },
  };
}

/* ------------------------------------------------------------------ *
 * Regressions
 * ------------------------------------------------------------------ */

test('applyRemoteClipboard resolves only after setData lands and rejects platform failures', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  harness.controller.start('session-a');
  await drain();

  let ackSettled = false;
  const apply = harness.controller.applyRemoteClipboard('session-a', { text: 'remote-text' });
  assert.ok(isThenable(apply), 'applyRemoteClipboard must return a promise');
  apply.then(
    () => { ackSettled = true; },
    () => { ackSettled = true; },
  );

  await waitUntil(() => harness.pasteboard.dispatchCount() === 1, 'the text write reaching setData');
  await drain(2);
  assert.equal(ackSettled, false, 'the ACK must wait for the platform write');
  assert.equal(harness.pasteboard.currentText(), '', 'nothing is published while setData is pending');

  assert.equal(harness.pasteboard.commitWrite(), true, 'the pending write must be committable');
  await apply;
  assert.equal(ackSettled, true, 'the ACK must resolve once setData completes');
  assert.equal(harness.pasteboard.currentText(), 'remote-text');

  const failing = harness.controller.applyRemoteClipboard('session-a', { text: 'second' });
  await waitUntil(() => harness.pasteboard.pendingWrites() === 1, 'the second write reaching setData');
  assert.equal(harness.pasteboard.rejectWrite(new Error('pasteboard setData failed')), true);
  await expectRejection(() => failing, 'a platform setData failure');
  assert.equal(harness.pasteboard.currentText(), 'remote-text', 'a failed write must not alter the pasteboard');
});

test('applyRemoteClipboard rejects a session that is not the active clipboard session', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  harness.controller.start('session-a');
  await drain();

  await expectRejection(
    () => harness.controller.applyRemoteClipboard('session-b', { text: 'foreign-session' }),
    'a write for a session that never started',
  );
  assert.equal(harness.pasteboard.dispatchCount(), 0, 'a wrong-session payload must never reach setData');
  assert.equal(harness.pasteboard.currentText(), '');
});

test('applyRemoteClipboard rejects unsupported payloads and publishes nothing', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  harness.controller.start('session-a');
  await drain();

  await expectRejection(
    () => harness.controller.applyRemoteClipboard('session-a', {
      image: { format: 'tiff', width: 2, height: 2, bytes: new Uint8Array(16) },
    }),
    'an unsupported image format',
  );
  assert.equal(harness.pasteboard.dispatchCount(), 0, 'an unsupported image must never reach setData');
  assert.equal(harness.image.decodeStarted(), 0, 'an unsupported format must not be handed to the decoder');

  await settleCall(() => harness.controller.applyRemoteClipboard('session-a', {}));
  assert.equal(harness.pasteboard.dispatchCount(), 0, 'an empty payload must never reach setData');
  assert.equal(harness.pasteboard.currentText(), '');
});

test('session A keeps its own incoming root after B is prepared and rejects B paths', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  harness.controller.start('session-a');
  const rootA = harness.controller.incomingFileRoot('session-a');
  const rootB = harness.controller.incomingFileRoot('session-b');
  assert.equal(typeof rootA, 'string');
  assert.notEqual(rootA, rootB, 'each session must own a distinct incoming root');
  harness.files.addDirectory(rootA);
  harness.files.addFile(`${rootA}/report.txt`, { size: 12 });
  harness.files.addDirectory(rootB);
  harness.files.addFile(`${rootB}/other.txt`, { size: 12 });
  await drain();

  await expectRejection(
    () => harness.controller.applyRemoteClipboard('session-a', { files: [`${rootB}/other.txt`] }),
    "a session B path inside session A's clipboard",
  );
  assert.equal(harness.pasteboard.dispatchCount(), 0, 'a foreign path must never be published');

  const accepted = harness.controller.applyRemoteClipboard('session-a', { files: [`${rootA}/report.txt`] });
  await waitUntil(() => harness.pasteboard.dispatchCount() === 1, "session A's own file reaching setData");
  assert.equal(harness.pasteboard.commitWrite(), true);
  await accepted;
  assert.deepEqual(harness.pasteboard.currentUris(), [`file://${rootA}/report.txt`]);
});

test('stop then start on the same session restores the pasteboard listener', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  harness.controller.start('session-a');
  assert.equal(harness.pasteboard.listenerCount(), 1, 'start must arm the update listener');

  harness.pasteboard.foreignText('local-one');
  await waitUntil(() => harness.texts().includes('local-one'), 'the first local copy reaching sendText');

  harness.controller.stop();
  assert.equal(harness.pasteboard.listenerCount(), 0, 'stop must detach the update listener');

  harness.pasteboard.foreignText('local-two');
  await drain(4);
  assert.ok(!harness.texts().includes('local-two'), 'a stopped controller must not read the local clipboard');

  harness.controller.start('session-a');
  assert.equal(harness.pasteboard.listenerCount(), 1, 'restarting the same session must re-arm the listener');

  harness.pasteboard.foreignText('local-three');
  await waitUntil(() => harness.texts().includes('local-three'), 'the local copy after restart reaching sendText');
});

test('a failed listener registration is surfaced and a retry re-arms the listener', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  harness.pasteboard.failNextListenerRegistration(new Error('pasteboard.on denied'));

  let thrown = null;
  try {
    harness.controller.start('session-a');
  } catch (error) {
    thrown = error;
  }
  assert.ok(
    thrown !== null || harness.logs().length > 0,
    'a failed listener registration must be surfaced by a throw or a diagnostic log',
  );
  assert.equal(harness.pasteboard.listenerCount(), 0, 'a failed registration must not leave a listener attached');

  harness.controller.start('session-a');
  assert.equal(harness.pasteboard.listenerCount(), 1, 'the retry must arm the update listener');

  harness.pasteboard.foreignText('after-retry');
  await waitUntil(() => harness.texts().includes('after-retry'), 'the local copy after the retried start');

  harness.controller.stop();
  assert.equal(harness.pasteboard.listenerCount(), 0);
});

test('the controller does not echo its own remote write back to the send callback', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  harness.controller.start('session-a');
  await drain();
  harness.resetCalls();

  const apply = harness.controller.applyRemoteClipboard('session-a', { text: 'remote-copy' });
  await waitUntil(() => harness.pasteboard.pendingWrites() === 1, 'the remote write reaching setData');
  assert.equal(harness.pasteboard.commitWrite(), true);
  await apply;
  await drain(8);

  assert.equal(harness.pasteboard.currentText(), 'remote-copy');
  assert.deepEqual(harness.texts(), [], 'the own write must be suppressed, not sent back as a local change');
});

test('a local copy during a pending remote write still reaches the send callback', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  harness.controller.start('session-a');
  await drain();
  harness.resetCalls();

  const apply = harness.controller.applyRemoteClipboard('session-a', { text: 'remote-copy' });
  await waitUntil(() => harness.pasteboard.pendingWrites() === 1, 'the remote write reaching setData');
  assert.equal(harness.pasteboard.commitWrite(), true);
  harness.pasteboard.foreignText('user-copy');

  await apply;
  await waitUntil(() => harness.texts().includes('user-copy'), 'the copy made during the remote write reaching sendText');
  assert.deepEqual(harness.texts(), ['user-copy'], 'the remote write itself must still be suppressed');
});

test('a stale image decode cannot publish and the next session still applies', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  harness.controller.start('session-a');
  await drain();

  const apply = harness.controller.applyRemoteClipboard('session-a', {
    image: { format: 'png', width: 2, height: 2, bytes: new Uint8Array([137, 80, 78, 71]) },
  });
  const rejected = assert.rejects(apply, /session changed while decoding/);
  await waitUntil(() => harness.image.decodeStarted() >= 1, 'the remote image decode starting');

  harness.controller.start('session-b');
  await harness.image.resolveDecode({ width: 2, height: 2 });
  await rejected;
  await drain(4);

  assert.equal(harness.pasteboard.dispatchCount(), 0, 'a decode finishing after the session changed must not publish');

  const next = harness.controller.applyRemoteClipboard('session-b', { text: 'after-stale' });
  await waitUntil(() => harness.pasteboard.pendingWrites() === 1, 'the next session write reaching setData');
  assert.equal(harness.pasteboard.commitWrite(), true);
  await next;
  assert.equal(harness.pasteboard.currentText(), 'after-stale');
});

test('stopAndDrainRemoteWrite waits for the in-flight write and detaches the listener', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  harness.controller.start('session-a');
  await drain();

  const apply = harness.controller.applyRemoteClipboard('session-a', { text: 'drain-me' });
  await waitUntil(() => harness.pasteboard.pendingWrites() === 1, 'the write reaching setData');

  let drained = false;
  const drainPromise = harness.controller.stopAndDrainRemoteWrite();
  drainPromise.then(
    () => { drained = true; },
    () => { drained = true; },
  );
  await drain(4);
  assert.equal(drained, false, 'the drain must not resolve while a write is in flight');
  assert.equal(harness.pasteboard.listenerCount(), 0, 'the drain must stop the session');

  assert.equal(harness.pasteboard.commitWrite(), true);
  await drainPromise;
  assert.equal(drained, true, 'the drain must resolve once the platform write completes');
  await settle(apply);
});

test('overlapping applies serialize and each caller awaits its own write', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  harness.controller.start('session-a');
  await drain();

  const first = harness.controller.applyRemoteClipboard('session-a', { text: 'first' });
  await waitUntil(() => harness.pasteboard.dispatchCount() === 1, 'the first write reaching setData');

  const second = harness.controller.applyRemoteClipboard('session-a', { text: 'second' });
  await drain(8);
  assert.equal(harness.pasteboard.dispatchCount(), 1, 'the second write must wait for the first to complete');
  assert.equal(harness.pasteboard.commitCount(), 0);

  assert.equal(harness.pasteboard.commitWrite(), true);
  await first;
  assert.deepEqual(harness.pasteboard.committedTexts(), ['first'], 'the first caller awaits its own write');

  await waitUntil(() => harness.pasteboard.dispatchCount() === 2, 'the queued write reaching setData');
  assert.equal(harness.pasteboard.commitWrite(), true);
  await second;
  assert.deepEqual(harness.pasteboard.committedTexts(), ['first', 'second']);
  assert.equal(harness.pasteboard.currentText(), 'second');
});

test('a failed write does not drop the queued apply', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  harness.controller.start('session-a');
  await drain();

  const first = harness.controller.applyRemoteClipboard('session-a', { text: 'one' });
  await waitUntil(() => harness.pasteboard.dispatchCount() === 1, 'the first write reaching setData');
  const second = harness.controller.applyRemoteClipboard('session-a', { text: 'two' });

  assert.equal(harness.pasteboard.rejectWrite(new Error('pasteboard busy')), true);
  await expectRejection(() => first, 'a failed first write');

  await waitUntil(() => harness.pasteboard.pendingWrites() === 1, 'the queued write being recovered');
  assert.equal(harness.pasteboard.commitWrite(), true);
  await second;
  assert.deepEqual(harness.pasteboard.committedTexts(), ['two']);
  assert.equal(harness.pasteboard.currentText(), 'two');
});

test('the controller keeps working after the ability context is detached', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  poisonContext(harness.context);

  const root = harness.controller.incomingFileRoot('session-a');
  assert.ok(
    root.startsWith(`${harness.filesDir}/`) && root.endsWith('/session-a'),
    `incomingFileRoot must use the snapshotted filesDir, got ${root}`,
  );

  harness.controller.start('session-a');
  harness.pasteboard.foreignText('after-detach');
  await waitUntil(() => harness.texts().includes('after-detach'), 'a local read after the context was detached');
  await harness.controller.stopAndDrainRemoteWrite();
});

test('an older published incoming root survives while a newer unpublished root is removed', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createHarness();
  const probeRoot = harness.controller.incomingFileRoot('session-probe');
  assert.ok(probeRoot.startsWith(`${harness.filesDir}/`), `unexpected incoming root ${probeRoot}`);
  const parent = probeRoot.slice(0, probeRoot.lastIndexOf('/'));
  const publishedRoot = `${parent}/session-old`;
  const unrelatedRoot = `${parent}/session-new`;
  harness.files.addDirectory(parent);
  harness.files.addDirectory(publishedRoot, { mtime: 1000 });
  harness.files.addFile(`${publishedRoot}/payload.bin`, { mtime: 1001, size: 8 });
  harness.files.addDirectory(unrelatedRoot, { mtime: 9000 });
  // Publish the URI in the same form the controller builds with fileUri.
  harness.pasteboard.foreignUris([
    harness.files.fileUri.getUriFromPath(`${publishedRoot}/payload.bin`),
  ]);

  const currentRoot = harness.controller.incomingFileRoot('session-current');
  assert.equal(currentRoot, `${parent}/session-current`);
  harness.controller.start('session-current');
  await waitUntil(() => !harness.files.has(unrelatedRoot), 'the unpublished incoming root to be pruned');

  assert.ok(
    harness.files.has(`${publishedRoot}/payload.bin`),
    'the root the pasteboard still publishes must survive cleanup even when another root is newer',
  );
  assert.ok(!harness.files.has(unrelatedRoot), 'an unpublished root must be removed');
});

/* ------------------------------------------------------------------ *
 * Bridge regressions (MethodChannel boundary)
 * ------------------------------------------------------------------ */

test('the bridge reports no clipboard session without an ability and arms one once attached', { timeout: TEST_TIMEOUT }, () => {
  const harness = createBridgeHarness();

  const unattached = harness.call('setClipboardSession', { sessionId: 'session-a' });
  assert.deepEqual(unattached.successes(), [false], 'a bridge without an ability must answer false');
  assert.deepEqual(unattached.errors(), [], 'a missing ability must not be reported as a clipboard error');

  const cleared = harness.call('setClipboardSession', { sessionId: '' });
  assert.deepEqual(cleared.successes(), [false], 'clearing a session without an ability must answer false');
  assert.deepEqual(cleared.errors(), []);

  const unprepared = harness.call('prepareClipboardSession', { sessionId: 'session-a' });
  assert.deepEqual(unprepared.successes(), [], 'no incoming root can be prepared without an ability');
  assert.deepEqual(unprepared.errors().map((error) => error.code), ['OHOS_CLIPBOARD_CONTEXT_MISSING']);

  const unwatched = harness.call('applyClipboardData', { sessionId: 'session-a', text: 'unwatched' });
  assert.deepEqual(unwatched.errors().map((error) => error.code), ['OHOS_CLIPBOARD_CONTEXT_MISSING']);
  assert.equal(harness.pasteboard.dispatchCount(), 0, 'an unarmed bridge must not publish to the pasteboard');

  harness.attach();
  const armed = harness.call('setClipboardSession', { sessionId: 'session-a' });
  assert.deepEqual(armed.successes(), [true], 'an attached bridge must arm the session');
  assert.deepEqual(armed.errors(), []);
  assert.equal(harness.pasteboard.listenerCount(), 1, 'arming a session must attach the pasteboard listener');
});

test('the bridge keeps its controller and prepared roots across ability recreation', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createBridgeHarness();
  harness.attach();

  const prepared = harness.call('prepareClipboardSession', { sessionId: 'session-a' });
  const root = prepared.successes()[0];
  assert.equal(root, `${harness.filesDir}/rustdesk-clipboard-in/session-a`);
  harness.files.addDirectory(`${harness.filesDir}/rustdesk-clipboard-in`);
  harness.files.addDirectory(root);
  harness.files.addFile(`${root}/payload.bin`, { size: 8 });

  const armed = harness.call('setClipboardSession', { sessionId: 'session-a' });
  assert.deepEqual(armed.successes(), [true]);
  assert.equal(harness.pasteboard.listenerCount(), 1);

  harness.detach();
  assert.equal(harness.pasteboard.listenerCount(), 0, 'detaching the ability must stop the clipboard listener');

  const readsBeforeDetachedPublish = harness.pasteboard.readCount();
  harness.pasteboard.foreignText('detached-local-copy');
  await drain(4);
  assert.equal(
    harness.pasteboard.readCount(),
    readsBeforeDetachedPublish,
    'a detached bridge must not read the local clipboard',
  );

  const detached = harness.call('setClipboardSession', { sessionId: 'session-a' });
  assert.deepEqual(detached.successes(), [false], 'a detached bridge must answer false');
  assert.deepEqual(detached.errors(), [], 'losing the ability must not be reported as a clipboard error');

  const detachedWrite = harness.call('applyClipboardData', { sessionId: 'session-a', text: 'detached-text' });
  assert.deepEqual(
    detachedWrite.errors().map((error) => error.code),
    ['OHOS_CLIPBOARD_CONTEXT_MISSING'],
    'a detached ability context must not be retained for clipboard writes',
  );
  assert.equal(harness.pasteboard.dispatchCount(), 0, 'a detached bridge must not publish to the pasteboard');

  harness.attach();
  const rearmed = harness.call('setClipboardSession', { sessionId: 'session-a' });
  assert.deepEqual(rearmed.successes(), [true], 'reattaching the ability must re-arm the same session');
  assert.equal(harness.pasteboard.listenerCount(), 1);

  const applied = harness.call('applyClipboardData', { sessionId: 'session-a', files: [`${root}/payload.bin`] });
  assert.equal(harness.pasteboard.dispatchCount(), 1, 'the retained root must still be publishable');
  assert.equal(harness.pasteboard.commitWrite(), true);
  await waitUntil(() => applied.settled, 'the file clipboard acknowledgement');
  assert.deepEqual(applied.successes(), [null], 'the landed file write must answer success');
  assert.deepEqual(harness.pasteboard.currentUris(), [`file://${root}/payload.bin`]);
});

test('applyClipboardData answers Dart only after the platform setData lands', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createBridgeHarness();
  harness.attach();
  const armed = harness.call('setClipboardSession', { sessionId: 'session-a' });
  assert.deepEqual(armed.successes(), [true]);

  const acknowledged = harness.call('applyClipboardData', { sessionId: 'session-a', text: 'remote-text' });
  assert.equal(harness.pasteboard.dispatchCount(), 1, 'the remote text must reach setData');
  assert.equal(acknowledged.settled, false, 'the Dart call must stay unanswered while setData is pending');
  await drain(4);
  assert.equal(acknowledged.settled, false, 'a pending setData must not be acknowledged');
  assert.equal(harness.pasteboard.currentText(), '', 'nothing is published while setData is pending');

  assert.equal(harness.pasteboard.commitWrite(), true);
  await waitUntil(() => acknowledged.settled, 'the write acknowledgement once setData lands');
  assert.deepEqual(acknowledged.successes(), [null], 'a landed write must answer success');
  assert.deepEqual(acknowledged.errors(), []);
  assert.equal(harness.pasteboard.currentText(), 'remote-text');
});

test('applyClipboardData decodes image-only payloads with omitted alternatives', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createBridgeHarness();
  harness.attach();
  harness.call('setClipboardSession', { sessionId: 'session-a' });

  const applied = harness.call('applyClipboardData', {
    sessionId: 'session-a',
    image: new Uint8Array([137, 80, 78, 71]),
    imageFormat: 'png',
  });
  assert.deepEqual(applied.errors(), []);
  assert.equal(harness.image.decodeStarted(), 1);
  await harness.image.resolveDecode({ width: 2, height: 2 });
  await waitUntil(() => harness.pasteboard.pendingWrites() === 1, 'the decoded image reaching setData');
  assert.equal(applied.settled, false);
  assert.equal(harness.pasteboard.commitWrite(), true);
  await waitUntil(() => applied.settled, 'the image clipboard acknowledgement');
  assert.deepEqual(applied.successes(), [null]);
  assert.deepEqual(applied.errors(), []);
});

test('applyClipboardData distinguishes absent content from explicit empty text', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createBridgeHarness();
  harness.attach();
  harness.call('setClipboardSession', { sessionId: 'session-a' });

  const missing = harness.call('applyClipboardData', { sessionId: 'session-a' });
  await waitUntil(() => missing.settled, 'the absent content rejection');
  assert.deepEqual(missing.errors().map((error) => error.code), ['OHOS_CLIPBOARD_WRITE_ERROR']);
  assert.equal(harness.pasteboard.dispatchCount(), 0);

  const empty = harness.call('applyClipboardData', {
    sessionId: 'session-a', text: '', html: null, files: null, image: null,
  });
  assert.deepEqual(empty.errors(), []);
  assert.equal(harness.pasteboard.dispatchCount(), 1);
  assert.equal(harness.pasteboard.commitWrite(), true);
  await waitUntil(() => empty.settled, 'the explicit empty text acknowledgement');
  assert.deepEqual(empty.successes(), [null]);
});

test('applyClipboardData reports a failed setData as OHOS_CLIPBOARD_WRITE_ERROR', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createBridgeHarness();
  harness.attach();
  harness.call('setClipboardSession', { sessionId: 'session-a' });

  const failed = harness.call('applyClipboardData', { sessionId: 'session-a', text: 'rejected-text' });
  await waitUntil(() => harness.pasteboard.pendingWrites() === 1, 'the remote text reaching setData');
  assert.equal(harness.pasteboard.rejectWrite(new Error('pasteboard busy')), true);

  await waitUntil(() => failed.settled, 'the write failure acknowledgement');
  assert.deepEqual(failed.successes(), [], 'a failed platform write must never answer success');
  const errors = failed.errors();
  assert.equal(errors.length, 1, `expected exactly one error answer, received ${describe(failed.calls)}`);
  assert.equal(errors[0].code, 'OHOS_CLIPBOARD_WRITE_ERROR');
  assert.match(errors[0].message, /pasteboard busy/, 'the platform failure must reach Dart');
  assert.equal(harness.pasteboard.currentText(), '', 'a failed write must not publish anything');
});

test('an empty setClipboardSession answers only once the in-flight write drains', { timeout: TEST_TIMEOUT }, async () => {
  const harness = createBridgeHarness();
  harness.attach();
  harness.call('setClipboardSession', { sessionId: 'session-a' });

  const inFlight = harness.call('applyClipboardData', { sessionId: 'session-a', text: 'drain-me' });
  await waitUntil(() => harness.pasteboard.pendingWrites() === 1, 'the remote text reaching setData');

  const drained = harness.call('setClipboardSession', { sessionId: '' });
  assert.equal(drained.settled, false, 'a drained session must not be answered while a write is in flight');
  assert.equal(harness.pasteboard.listenerCount(), 0, 'an empty session must stop the controller at once');
  await drain(4);
  assert.equal(drained.settled, false, 'the empty session must wait for the platform write to drain');

  assert.equal(harness.pasteboard.commitWrite(), true);
  await waitUntil(() => drained.settled, 'the drained empty session answer');
  assert.deepEqual(drained.successes(), [false], 'a drained empty session must answer false');
  assert.deepEqual(drained.errors(), []);
  await waitUntil(() => inFlight.settled, 'the drained write acknowledgement');
  assert.deepEqual(inFlight.successes(), [null], 'the in-flight write still completes after the drain');
});
