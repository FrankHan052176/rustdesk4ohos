import 'dart:async';
import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:device_info_plus/device_info_plus.dart';
import 'package:ffi/ffi.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:flutter_hbb/main.dart';
import 'package:package_info_plus/package_info_plus.dart';
import 'package:path_provider/path_provider.dart';

import '../common.dart';
import '../generated_bridge.dart';
import 'ohos_clipboard_sync.dart';
import '../utils/ohos_session_window.dart';

final class RgbaFrame extends Struct {
  @Uint32()
  external int len;
  external Pointer<Uint8> data;
}

const _kOhosHostInputCapable = 'ohos-host-input-capable';

typedef F3 = Pointer<Uint8> Function(Pointer<Utf8>, int);
typedef F3Dart = Pointer<Uint8> Function(Pointer<Utf8>, Int32);
typedef HandleEvent = Future<void> Function(Map<String, dynamic> evt);

/// The Linux bundle keeps the core library at lib/librustdesk.so next to the
/// executable. Prefer that copy, mirroring flutter/linux/main.cc: the plain
/// name relies on the loader search path, which repackaged installs may not
/// cover. https://github.com/rustdesk/rustdesk/discussions/14407
DynamicLibrary _openLinuxCoreLib() {
  final bundled =
      '${File(Platform.resolvedExecutable).parent.path}/lib/librustdesk.so';
  try {
    if (File(bundled).existsSync()) {
      return DynamicLibrary.open(bundled);
    }
  } catch (e) {
    debugPrint("Failed to load '$bundled': $e");
  }
  return DynamicLibrary.open('librustdesk.so');
}

/// FFI wrapper around the native Rust core.
/// Hides the platform differences.
class PlatformFFI {
  static String _ohosLocaleName = '';
  String _dir = '';
  // _homeDir is only needed for Android and IOS.
  String _homeDir = '';
  int _ohosDisplayId = 0;
  int _ohosDisplayWidth = 0;
  int _ohosDisplayHeight = 0;
  final _eventHandlers = <String, Map<String, HandleEvent>>{};
  late RustdeskImpl _ffiBind;
  late String _appType;
  StreamEventHandler? _eventCallback;
  StreamSubscription<String>? _globalEventSubscription;
  Future<void>? _windowCleanup;

  PlatformFFI._();

  static final PlatformFFI instance = PlatformFFI._();
  final _toAndroidChannel = const MethodChannel('mChannel');
  final _toOhosChannel =
      const MethodChannel('top.frankhan.resk.flutter/platform');
  bool _ohosClipboardAllowed = false;
  int _ohosClipboardRequest = 0;
  Future<void> _ohosClipboardUpdates = Future<void>.value();
  bool _ohosHostClipboardActive = false;
  bool _ohosHostClipboardRootReady = false;
  bool _ohosClientClipboardActive = false;
  bool _ohosClipboardPolling = false;
  String _ohosClipboardSession = '';
  int _ohosClipboardEpoch = 0;
  final _ohosClipboardSessions = <String, Future<void>>{};
  final _ohosClipboardFiles = <String, List<String>>{};
  late final _ohosClipboardSync = OhosClipboardSync(
    readLocal: (force) => _toOhosChannel
        .invokeMethod<String>('getClipboardText', {'force': force}),
    writeLocal: (text) =>
        _toOhosChannel.invokeMethod<void>('setClipboardText', {'text': text}),
    sendLocal: _sendOhosClipboard,
    takeRemote: _takeOhosClipboard,
  );

  RustdeskImpl get ffiBind => _ffiBind;
  F3? _session_get_rgba;

  static String get localeName => isOhos && _ohosLocaleName.isNotEmpty
      ? _ohosLocaleName
      : Platform.localeName;

  static get isMain => instance._appType == kAppTypeMain;

  static String getByName(String name, [String arg = '']) {
    return '';
  }

  static void setByName(String name, [String value = '']) {}

  static Future<String> getVersion() async {
    if (isOhos) {
      return await instance._ffiBind.mainGetVersion();
    }
    PackageInfo packageInfo = await PackageInfo.fromPlatform();
    return packageInfo.version;
  }

  bool registerEventHandler(
      String eventName, String handlerName, HandleEvent handler,
      {bool replace = false}) {
    debugPrint('registerEventHandler $eventName $handlerName');
    var handlers = _eventHandlers[eventName];
    if (handlers == null) {
      _eventHandlers[eventName] = {handlerName: handler};
      return true;
    } else {
      if (!replace && handlers.containsKey(handlerName)) {
        return false;
      } else {
        handlers[handlerName] = handler;
        return true;
      }
    }
  }

  void unregisterEventHandler(String eventName, String handlerName) {
    debugPrint('unregisterEventHandler $eventName $handlerName');
    var handlers = _eventHandlers[eventName];
    if (handlers != null) {
      handlers.remove(handlerName);
    }
  }

  String translate(String name, String locale) =>
      _ffiBind.translate(name: name, locale: locale);

  Uint8List? getRgba(SessionID sessionId, int display, int bufSize) {
    if (_session_get_rgba == null) return null;
    final sessionIdStr = sessionId.toString();
    var a = sessionIdStr.toNativeUtf8();
    try {
      final buffer = _session_get_rgba!(a, display);
      if (buffer == nullptr) {
        return null;
      }
      final data = buffer.asTypedList(bufSize);
      return data;
    } finally {
      malloc.free(a);
    }
  }

  int getRgbaSize(SessionID sessionId, int display) =>
      _ffiBind.sessionGetRgbaSize(sessionId: sessionId, display: display);
  void nextRgba(SessionID sessionId, int display) =>
      _ffiBind.sessionNextRgba(sessionId: sessionId, display: display);
  void registerPixelbufferTexture(SessionID sessionId, int display, int ptr) =>
      _ffiBind.sessionRegisterPixelbufferTexture(
          sessionId: sessionId, display: display, ptr: ptr);
  void registerGpuTexture(SessionID sessionId, int display, int ptr) =>
      _ffiBind.sessionRegisterGpuTexture(
          sessionId: sessionId, display: display, ptr: ptr);

  /// Init the FFI class, loads the native Rust core library.
  Future<void> init(String appType) async {
    _appType = appType;
    final dylib = isOhos
        ? DynamicLibrary.open('liblibrustdesk.so')
        : isAndroid
            ? DynamicLibrary.open('librustdesk.so')
            : isLinux
                ? _openLinuxCoreLib()
                : isWindows
                    ? DynamicLibrary.open('librustdesk.dll')
                    :
                    // Use executable itself as the dynamic library for MacOS.
                    // Multiple dylib instances will cause some global instances to be invalid.
                    // eg. `lazy_static` objects in rust side, will be created more than once, which is not expected.
                    //
                    // isMacOS? DynamicLibrary.open("liblibrustdesk.dylib") :
                    DynamicLibrary.process();
    debugPrint('initializing FFI $_appType');
    try {
      _session_get_rgba = dylib.lookupFunction<F3Dart, F3>("session_get_rgba");
      try {
        if (isOhos) {
          _dir = await _toOhosChannel.invokeMethod<String>('getFilesDir') ?? '';
          ohosDeviceType =
              await _toOhosChannel.invokeMethod<String>('getDeviceType') ?? '';
          _ohosLocaleName =
              await _toOhosChannel.invokeMethod<String>('getSystemLocale') ??
                  '';
          final displayInfo = await _toOhosChannel
              .invokeMapMethod<String, dynamic>('getDefaultDisplayInfo');
          _ohosDisplayId = displayInfo?['displayId'] as int? ?? 0;
          _ohosDisplayWidth = displayInfo?['width'] as int? ?? 0;
          _ohosDisplayHeight = displayInfo?['height'] as int? ?? 0;
          _toOhosChannel.setMethodCallHandler(_handleOhosClipboardCall);
          _applyOhosWindowStatus(
              await _toOhosChannel.invokeMethod<String>('watchWindowStatus'));
          // A free-form window on a tablet keeps its caption as well, so every OHOS window
          // gets the same framing preparation.
          await _toOhosChannel.invokeMethod<void>('prepareWindow');
        } else {
          // SYSTEM user failed
          _dir = (await getApplicationDocumentsDirectory()).path;
        }
      } catch (e) {
        debugPrint('Failed to get documents directory: $e');
      }
      if (_dir.isEmpty) {
        throw StateError('Application files directory is unavailable');
      }
      _ffiBind = RustdeskImpl(dylib);

      if (isLinux) {
        if (isMain) {
          // Start a dbus service for uri links, no need to await
          _ffiBind.mainStartDbusServer();
        }
      } else if (isMacOS && isMain) {
        // Start ipc service for uri links.
        _ffiBind.mainStartIpcUrlServer();
      }
      _startListenEvent(_ffiBind); // global event
      try {
        if (isAndroid) {
          // Android file transfer uses app-specific storage. User-selected
          // files enter and leave this workspace through the system picker.
          _homeDir = (await getExternalStorageDirectory())?.path ??
              (await getApplicationSupportDirectory()).path;
        } else if (isIOS) {
          // The previous code was `_homeDir = (await getDownloadsDirectory())?.path ?? '';`,
          // which provided the `downloads` path in the sandbox.
          // It is unclear why we now use the `data` directory in the sandbox instead.
          _homeDir = _ffiBind.mainGetDataDirIos(appDir: _dir);
        } else if (isOhos) {
          _homeDir = _dir;
        } else {
          // no need to set home dir
        }
      } catch (e) {
        debugPrintStack(label: 'initialize failed: $e');
      }
      String id = 'NA';
      String name = 'Flutter';
      DeviceInfoPlugin deviceInfo = DeviceInfoPlugin();
      if (isAndroid) {
        AndroidDeviceInfo androidInfo = await deviceInfo.androidInfo;
        name = '${androidInfo.brand}-${androidInfo.model}';
        id = androidInfo.id.hashCode.toString();
        androidVersion = androidInfo.version.sdkInt;
      } else if (isIOS) {
        IosDeviceInfo iosInfo = await deviceInfo.iosInfo;
        name = iosInfo.utsname.machine;
        id = iosInfo.identifierForVendor.hashCode.toString();
      } else if (isOhos) {
        name = Platform.localHostname;
        id = name.hashCode.toString();
      } else if (isLinux) {
        LinuxDeviceInfo linuxInfo = await deviceInfo.linuxInfo;
        name = linuxInfo.name;
        id = linuxInfo.machineId ?? linuxInfo.id;
      } else if (isWindows) {
        try {
          // request windows build number to fix overflow on win7
          windowsBuildNumber = getWindowsTargetBuildNumber();
          WindowsDeviceInfo winInfo = await deviceInfo.windowsInfo;
          name = winInfo.computerName;
          id = winInfo.computerName;
        } catch (e) {
          debugPrintStack(label: "get windows device info failed: $e");
          name = "unknown";
          id = "unknown";
        }
      } else if (isMacOS) {
        MacOsDeviceInfo macOsInfo = await deviceInfo.macOsInfo;
        name = macOsInfo.computerName;
        id = macOsInfo.systemGUID ?? '';
      }
      if (isAndroid || isIOS || isOhos) {
        debugPrint(
            '_appType:$_appType,info1-id:$id,info2-name:$name,dir:$_dir,homeDir:$_homeDir');
      } else {
        debugPrint(
            '_appType:$_appType,info1-id:$id,info2-name:$name,dir:$_dir');
      }
      if (desktopType == DesktopType.cm) {
        await _ffiBind.cmInit();
      }
      await _ffiBind.mainDeviceId(id: id);
      await _ffiBind.mainDeviceName(name: name);
      await _ffiBind.mainSetHomeDir(home: _homeDir);
      await _ffiBind.mainInit(
        appDir: _dir,
        customClientConfig: '',
      );
      if (isOhos && isMain) {
        await _ffiBind.mainSetLocalOption(
          key: _kOhosHostInputCapable,
          value: isOhosDesktop ? 'Y' : 'N',
        );
        final configured = await _ffiBind.mainConfigureOhosHostDisplay(
          width: _ohosDisplayWidth,
          height: _ohosDisplayHeight,
          displayId: _ohosDisplayId,
        );
        debugPrint('OHOS host display configured: $configured');
        final stopError = await _ffiBind.mainStopOhosHost();
        if (stopError.isNotEmpty) {
          debugPrint('Failed to initialize OHOS host state: $stopError');
        }
      }
    } catch (e) {
      debugPrintStack(label: 'initialize failed: $e');
    }
    version = await getVersion();
  }

  Future<bool> tryHandle(Map<String, dynamic> evt) async {
    final name = evt['name'];
    if (name != null) {
      final handlers = _eventHandlers[name];
      if (handlers != null) {
        if (handlers.isNotEmpty) {
          for (var handler in handlers.values) {
            await handler(evt);
          }
          return true;
        }
      }
    }
    return false;
  }

  /// Start listening to the Rust core's events and frames.
  void _startListenEvent(RustdeskImpl rustdeskImpl) {
    final appType =
        _appType == kAppTypeDesktopRemote ? '$_appType,$kWindowId' : _appType;
    var sink = rustdeskImpl.startGlobalEventStream(appType: appType);
    _globalEventSubscription = sink.listen((message) {
      () async {
        try {
          Map<String, dynamic> event = json.decode(message);
          // _tryHandle here may be more flexible than _eventCallback
          if (!await tryHandle(event)) {
            if (_eventCallback != null) {
              await _eventCallback!(event);
            }
          }
        } catch (e) {
          debugPrint('json.decode fail(): $e');
        }
      }();
    });
  }

  void setEventCallback(StreamEventHandler fun) async {
    _eventCallback = fun;
  }

  Future<Map<String, dynamic>?> getWindowLaunchPayload() {
    _toOhosChannel.setMethodCallHandler(_handleOhosClipboardCall);
    return _toOhosChannel.invokeMapMethod<String, dynamic>('getWindowLaunchPayload');
  }

  Future<void> openSessionWindow(Map<String, dynamic> payload, String title) async {
    await _toOhosChannel.invokeMethod<String>('openSessionWindow', {
      'payload': jsonEncode(payload),
      'title': title,
    });
  }

  Future<void> prepareSessionWindow() =>
      _toOhosChannel.invokeMethod<void>('prepareWindow');

  Future<void> _cleanupSessionWindow() =>
      _windowCleanup ??= _finishSessionWindow();

  Future<void> _finishSessionWindow() async {
    await OhosSessionWindow.close();
    if (_globalEventSubscription != null) {
      await _ffiBind.stopGlobalEventStream(appType: _appType);
      await _globalEventSubscription!.cancel();
      _globalEventSubscription = null;
    }
  }

  void setRgbaCallback(void Function(int, Uint8List) fun) async {}

  // web only, decoded WebCodecs frames arriving as ready-made images
  void setVideoFrameCallback(
      Future<void> Function(int, ui.Image, bool Function()) fun) {}

  void clearVideoFrameCallback() {}

  void startDesktopWebListener() {}

  void stopDesktopWebListener() {}

  Future<List<String>> getSoundInputs() async {
    if (isOhos) {
      return (await _toOhosChannel
                  .invokeListMethod<String>('getAudioInputDevices') ??
              <String>[])
          .where((device) => device.isNotEmpty)
          .toList();
    }
    return (await _ffiBind.mainGetSoundInputs())
        .where((device) => device.isNotEmpty)
        .toList();
  }

  Future<void> selectSoundInput(String device) async {
    if (!isOhos) return;
    await _toOhosChannel
        .invokeMethod<void>('selectAudioInputDevice', {'device': device});
  }

  Future<bool> isWindowMaximized() async {
    if (!isOhos) return false;
    return await _toOhosChannel.invokeMethod<bool>('isWindowMaximized') ??
        false;
  }

  Future<void> minimizeWindow() async {
    if (!isOhos) return;
    await _toOhosChannel.invokeMethod<void>('minimizeWindow');
  }

  Future<bool> toggleMaximizeWindow() async {
    if (!isOhos) return false;
    return await _toOhosChannel.invokeMethod<bool>('toggleMaximizeWindow') ??
        false;
  }

  Future<void> startMovingWindow() async {
    if (!isOhos) return;
    await _toOhosChannel.invokeMethod<void>('startMovingWindow');
  }

  Future<void> setKeepScreenOn(bool enabled) async {
    if (!isOhos) return;
    await _toOhosChannel
        .invokeMethod<void>('setKeepScreenOn', {'enabled': enabled});
  }

  /// Native fullscreen for the main window or a 2in1 session window.
  Future<void> setFullscreen(bool enabled) async {
    if (!isOhos) return;
    await _toOhosChannel
        .invokeMethod<void>('setFullscreen', {'enabled': enabled});
  }

  Future<String> getOhosDownloadDirectory() async {
    if (!isOhos) {
      throw UnsupportedError('HarmonyOS Download directory is unavailable');
    }
    final directory =
        await _toOhosChannel.invokeMethod<String>('getDownloadDirectory');
    if (directory == null || directory.isEmpty) {
      throw StateError('Application Download directory is unavailable');
    }
    return directory;
  }

  Future<void> _updateOhosClipboardCapability() {
    final update = _ohosClipboardUpdates.then((_) => _ffiBind
        .mainSetOhosHostClipboardEnabled(enabled: _ohosClipboardAllowed));
    // Preserve ordering after a failure; callers still receive the original error.
    _ohosClipboardUpdates = update.catchError((Object error) {
      debugPrint('Failed to update OHOS clipboard capability: $error');
    });
    return update;
  }

  Future<bool> setOhosClipboardEnabled(bool enabled,
      {bool requestPermission = false}) async {
    if (!isOhos) return false;
    final request = ++_ohosClipboardRequest;
    final granted = enabled &&
        (await _toOhosChannel.invokeMethod<bool>(requestPermission
                ? 'requestClipboardPermission'
                : 'hasClipboardPermission') ??
            false);
    if (request != _ohosClipboardRequest) return _ohosClipboardAllowed;
    _ohosClipboardAllowed = granted;
    if (!granted && isOhosDesktop) {
      _ohosClipboardEpoch++;
      _ohosClipboardSession = '';
      _ohosHostClipboardActive = false;
      _ohosHostClipboardRootReady = false;
      await _toOhosChannel
          .invokeMethod<bool>('setClipboardSession', {'sessionId': ''});
    }
    if (!granted) _ohosClipboardSync.enabled = false;
    await _updateOhosClipboardCapability();
    return _ohosClipboardAllowed;
  }

  Future<void> prepareOhosClipboardSession(String sessionId) {
    if (!isOhosDesktop) return Future<void>.value();
    return _ohosClipboardSessions.putIfAbsent(sessionId, () async {
      final root = await _toOhosChannel.invokeMethod<String>(
          'prepareClipboardSession', {'sessionId': sessionId});
      if (root == null ||
          root.isEmpty ||
          !await _ffiBind.mainSetOhosClientClipboardFileRoot(
              sessionId: sessionId, root: root)) {
        throw StateError('Unable to prepare the session clipboard directory');
      }
    });
  }

  Future<void> closeOhosClipboardSession(String sessionId) async {
    if (!isOhosDesktop) return;
    _ohosClipboardEpoch++;
    if (_ohosClipboardSession == sessionId) _ohosClipboardSession = '';
    final preparing = _ohosClipboardSessions.remove(sessionId);
    _ohosClipboardFiles.remove(sessionId);
    try {
      await preparing;
    } catch (error) {
      debugPrint('OHOS clipboard preparation failed (${error.runtimeType})');
    }
    await _toOhosChannel
        .invokeMethod<void>('closeClipboardSession', {'sessionId': sessionId});
  }

  Future<bool> _handleOhosClipboardCall(MethodCall call) async {
    if (call.method == 'onWindowClose') {
      if (!OhosSessionWindow.isSession) return false;
      await _cleanupSessionWindow();
      return true;
    }
    if (call.method == 'onWindowStatus') {
      _applyOhosWindowStatus((call.arguments as Map)['status'] as String?);
      return true;
    }
    final args = Map<String, dynamic>.from(call.arguments as Map);
    final sessionId = args['sessionId'] as String;
    if (sessionId.isEmpty || sessionId != _ohosClipboardSession) return false;
    final hostActive = _ohosHostClipboardActive;
    switch (call.method) {
      case 'onClipboardText':
        final text = args['text'] as String;
        final accepted = await _ffiBind.mainSendOhosClipboardText(
            sessionId: sessionId, text: text, hostActive: hostActive);
        if (!accepted || !hostActive || sessionId == 'host') return accepted;
        return _ffiBind.mainSendOhosClipboardText(
            sessionId: 'host', text: text, hostActive: hostActive);
      case 'onClipboardHtml':
        final html = args['html'] as String;
        final text = args['text'] as String;
        final accepted = await _ffiBind.mainSendOhosClipboardHtml(
            sessionId: sessionId, html: html, text: text, hostActive: hostActive);
        if (!accepted || !hostActive || sessionId == 'host') return accepted;
        return _ffiBind.mainSendOhosClipboardHtml(
            sessionId: 'host', html: html, text: text, hostActive: hostActive);
      case 'onClipboardImage':
        final image = args['image'] as Uint8List;
        final accepted = await _ffiBind.mainSendOhosClipboardImage(
            sessionId: sessionId, png: image, hostActive: hostActive);
        if (!accepted || !hostActive || sessionId == 'host') return accepted;
        return _ffiBind.mainSendOhosClipboardImage(
            sessionId: 'host', png: image, hostActive: hostActive);
      case 'onClipboardFiles':
        return _ffiBind.mainSendOhosClipboardFiles(
            sessionId: sessionId,
            paths: List<String>.from(args['paths'] as List));
      default:
        throw MissingPluginException('Unsupported clipboard callback');
    }
  }

  Future<void> applyOhosClipboardFiles(
      String sessionId, List<String> files) async {
    if (!isOhosDesktop ||
        !_ohosClipboardSessions.containsKey(sessionId) ||
        files.isEmpty) {
      return;
    }
    if (sessionId != _ohosClipboardSession) {
      _ohosClipboardFiles[sessionId] = files;
      return;
    }
    await _toOhosChannel.invokeMethod<void>('applyClipboardData', {
      'sessionId': sessionId,
      'files': files,
    });
  }

  /// Places a locally captured picture (for example a remote screenshot the
  /// user chose to copy) into the HarmonyOS pasteboard for the active session.
  Future<void> applyOhosClipboardImage(
      String sessionId, Uint8List png) async {
    if (!isOhosDesktop || sessionId.isEmpty || png.isEmpty) return;
    await prepareOhosClipboardSession(sessionId);
    if (sessionId != _ohosClipboardSession) {
      debugPrint('OHOS clipboard image skipped: session is not armed');
      return;
    }
    await _toOhosChannel.invokeMethod<void>('applyClipboardData', {
      'sessionId': sessionId,
      'image': png,
      'imageFormat': 'png',
      'width': 0,
      'height': 0,
    });
  }

  /// Arms the controlled-side incoming-file directory so a controller can paste files.
  Future<void> _prepareOhosHostClipboardSession() async {
    if (_ohosHostClipboardRootReady) return;
    final root = await _toOhosChannel.invokeMethod<String>(
        'prepareClipboardSession', {'sessionId': 'host'});
    if (root == null || root.isEmpty) {
      throw StateError('Unable to prepare the controlled clipboard directory');
    }
    if (!await _ffiBind.mainSetOhosHostClipboardFileRoot(root: root)) {
      throw StateError('Unable to register the controlled clipboard directory');
    }
    _ohosHostClipboardRootReady = true;
  }

  /// Publishes files a controller pasted into the controlled device's clipboard.
  Future<void> _applyOhosHostClipboardFiles() async {
    final files = await _ffiBind.mainTakeOhosHostClipboardFiles();
    if (files.isEmpty) return;
    await _toOhosChannel.invokeMethod<void>('applyClipboardData', {
      'sessionId': 'host',
      'files': files,
    });
  }

  Future<void> _syncOhosExtendedClipboard(bool hostActive) async {
    final epoch = _ohosClipboardEpoch;
    final clientId = await _ffiBind.mainGetOhosClipboardSession();
    final hasPermission =
        await _toOhosChannel.invokeMethod<bool>('hasClipboardPermission') ??
            false;
    if (epoch != _ohosClipboardEpoch) return;
    _ohosHostClipboardActive =
        hostActive && _ohosClipboardAllowed && hasPermission;
    final sessionId = hasPermission && clientId.isNotEmpty
        ? clientId
        : _ohosHostClipboardActive
            ? 'host'
            : '';
    if (sessionId != _ohosClipboardSession) {
      if (sessionId == 'host') {
        await _prepareOhosHostClipboardSession();
        if (epoch != _ohosClipboardEpoch) return;
      } else if (sessionId.isNotEmpty) {
        await prepareOhosClipboardSession(sessionId);
        if (epoch != _ohosClipboardEpoch) return;
      }
      _ohosClipboardSession = sessionId;
    }
    // Ability recreation pauses the native listener without changing Dart's
    // session id. Re-arm idempotently rather than trusting that cached id.
    final armed = await _toOhosChannel
        .invokeMethod<bool>('setClipboardSession', {'sessionId': sessionId});
    if (epoch != _ohosClipboardEpoch || armed != true) return;
    if (sessionId.isEmpty) return;
    final files = _ohosClipboardFiles.remove(sessionId);
    if (files != null) {
      await applyOhosClipboardFiles(sessionId, files);
      if (epoch != _ohosClipboardEpoch) return;
    }
    if (sessionId == 'host') {
      await _applyOhosHostClipboardFiles();
      if (epoch != _ohosClipboardEpoch) return;
    }
    var data = await _ffiBind.mainTakeOhosClipboardData(
        sessionId: sessionId, hostActive: _ohosHostClipboardActive);
    if (data == null && _ohosHostClipboardActive && sessionId != 'host') {
      data = await _ffiBind.mainTakeOhosClipboardData(
          sessionId: 'host', hostActive: true);
    }
    if (data == null ||
        epoch != _ohosClipboardEpoch ||
        sessionId != _ohosClipboardSession) {
      return;
    }
    await _toOhosChannel.invokeMethod<void>('applyClipboardData', {
      'sessionId': sessionId,
      if (data.text != null) 'text': data.text,
      if (data.html != null) 'html': data.html,
      if (data.image.isNotEmpty) ...{
        'image': data.image,
        'imageFormat': data.imageFormat,
        'width': data.width,
        'height': data.height,
      },
    });
  }

  Future<bool> _sendOhosClipboard(String text) async {
    final hostAccepted = !_ohosHostClipboardActive ||
        await _ffiBind.mainUpdateOhosHostClipboardText(text: text);
    final clientAccepted = !_ohosClientClipboardActive ||
        await _ffiBind.mainUpdateOhosClientClipboardText(text: text);
    return hostAccepted && clientAccepted;
  }

  Future<String?> _takeOhosClipboard() async {
    if (_ohosClientClipboardActive) {
      final text = await _ffiBind.mainTakeOhosClientClipboardText();
      if (text != null) return text;
    }
    return _ohosHostClipboardActive
        ? _ffiBind.mainTakeOhosHostClipboardText()
        : null;
  }

  Future<bool> syncOhosClipboard({required bool hostActive}) async {
    if (!isOhos) return false;
    if (_ohosClipboardPolling) return true;
    _ohosClipboardPolling = true;
    try {
      if (isOhosDesktop) {
        await _syncOhosExtendedClipboard(hostActive);
        return true;
      }
      final clientRequired = await _ffiBind.mainOhosClientClipboardRequired();
      final clientActive = clientRequired &&
          (await _toOhosChannel.invokeMethod<bool>('hasClipboardPermission') ??
              false);
      final allowedHost = hostActive && _ohosClipboardAllowed;
      if (_ohosHostClipboardActive != allowedHost ||
          _ohosClientClipboardActive != clientActive) {
        _ohosClipboardSync.enabled = false;
      }
      _ohosHostClipboardActive = allowedHost;
      _ohosClientClipboardActive = clientActive;
      _ohosClipboardSync.enabled = allowedHost || clientActive;
      await _ohosClipboardSync.synchronize();
      return true;
    } catch (error) {
      debugPrint(
          'OHOS clipboard synchronization failed (${error.runtimeType})');
      await setOhosClipboardEnabled(false);
      return false;
    } finally {
      _ohosClipboardPolling = false;
    }
  }

  Future<String> startOhosHost() async {
    if (!isOhos) return 'OHOS host is unavailable';
    await _ffiBind.mainSetLocalOption(
      key: _kOhosHostInputCapable,
      value: isOhosDesktop ? 'Y' : 'N',
    );
    try {
      final displayInfo = await _toOhosChannel
          .invokeMapMethod<String, dynamic>('getDefaultDisplayInfo');
      final displayId = displayInfo?['displayId'] as int? ?? 0;
      final width = displayInfo?['width'] as int? ?? 0;
      final height = displayInfo?['height'] as int? ?? 0;
      if (width > 0 && height > 0) {
        _ohosDisplayId = displayId;
        _ohosDisplayWidth = width;
        _ohosDisplayHeight = height;
        final configured = await _ffiBind.mainConfigureOhosHostDisplay(
          width: width,
          height: height,
          displayId: displayId,
        );
        if (!configured) {
          return 'Failed to configure the HarmonyOS host display';
        }
      }
    } catch (error) {
      return 'Failed to read the HarmonyOS display: $error';
    }
    final clipboardOption =
        await _ffiBind.mainGetOption(key: kOptionEnableClipboard);
    await setOhosClipboardEnabled(clipboardOption != 'N');
    final error = await _ffiBind.mainStartOhosHost();
    if (error.isNotEmpty) {
      await setOhosClipboardEnabled(false);
      return error;
    }
    try {
      await _toOhosChannel.invokeMethod<void>('startContinuousTask');
      return '';
    } catch (error) {
      await setOhosClipboardEnabled(false);
      await _ffiBind.mainStopOhosHost();
      return 'Failed to start the HarmonyOS continuous task: $error';
    }
  }

  Future<String> stopOhosHost() async {
    if (!isOhos) return '';
    await setOhosClipboardEnabled(false);
    final error = await _ffiBind.mainStopOhosHost();
    try {
      await _toOhosChannel.invokeMethod<void>('stopContinuousTask');
    } catch (backgroundError) {
      if (error.isEmpty) {
        return 'Failed to stop the HarmonyOS continuous task: $backgroundError';
      }
    }
    return error;
  }
  Future<void> closeWindow() async {
    if (!isOhos) return;
    if (OhosSessionWindow.isSession) {
      // A failed teardown still leaves the user a way out of a stuck window.
      try {
        await _cleanupSessionWindow();
      } catch (error) {
        debugPrint('OHOS session cleanup failed (${error.runtimeType})');
      }
    }
    await _toOhosChannel.invokeMethod<void>('closeWindow');
  }

  /// Closes the HarmonyOS window without running the session teardown first, for callers
  /// that are already inside it.
  Future<void> terminateWindow() async {
    if (!isOhos) return;
    await _toOhosChannel.invokeMethod<void>('closeWindow');
  }

  String _ohosRecordingDirectory = '';

  /// Makes sure recordings land in a writable, user-visible directory, asking the user for
  /// one with the system folder picker the first time.
  Future<String> ensureOhosRecordingDirectory() async {
    if (!isOhos) return '';
    if (_ohosRecordingDirectory.isNotEmpty) return _ohosRecordingDirectory;
    try {
      final directory = await getOhosDownloadDirectory();
      if (directory.isNotEmpty &&
          await _ffiBind.mainSetOhosRecordingDirectory(path: directory)) {
        _ohosRecordingDirectory = directory;
      }
    } catch (error) {
      debugPrint(
          'OHOS recording directory unavailable (${error.runtimeType}); keeping the app default');
    }
    return _ohosRecordingDirectory;
  }

  /// Applies the HarmonyOS window mode; a free-form window uses the desktop UI.
  void _applyOhosWindowStatus(String? status) {
    if (!isOhos) return;
    final freeform = status == 'floating';
    debugPrint('OHOS window mode: ${status ?? 'unknown'}, freeform=$freeform');
    if (ohosFreeformWindow.value != freeform) {
      ohosFreeformWindow.value = freeform;
    }
  }

  void setMethodCallHandler(FMethod callback) {
    _toAndroidChannel.setMethodCallHandler((call) async {
      callback(call.method, call.arguments);
      return null;
    });
  }

  invokeMethod(String method, [dynamic arguments]) async {
    if (isOhos && method == 'enable_soft_keyboard') {
      // The Android channel owns the soft-keyboard hint; on HarmonyOS the session only
      // needs the input method to really go away when it hides the keyboard. Leaving the
      // IME up also kept the keyboard inset, which lifted the bottom bar.
      if (arguments == false) {
        // clearClient drops the input connection, which is what actually dismisses the
        // HarmonyOS input method; hide() alone leaves it on screen.
        debugPrint('OHOS soft keyboard: dismissing the input method');
        await SystemChannels.textInput.invokeMethod<void>('TextInput.clearClient');
        await SystemChannels.textInput.invokeMethod<void>('TextInput.hide');
      }
      return true;
    }
    if (!isAndroid) return Future<bool>(() => false);
    return await _toAndroidChannel.invokeMethod(method, arguments);
  }

  Future<T?> invokeMethodWithResult<T>(String method,
      [dynamic arguments]) async {
    if (!isAndroid) return null;
    return await _toAndroidChannel.invokeMethod<T>(method, arguments);
  }

  void syncAndroidServiceAppDirConfigPath() {
    invokeMethod(AndroidChannel.kSyncAppDirConfigPath, _dir);
  }

  void setFullscreenCallback(void Function(bool) fun) {}
}
