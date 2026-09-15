import { abilityAccessCtrl, common, Context, Permissions, wantAgent, WantAgent } from '@kit.AbilityKit';
import { audio } from '@kit.AudioKit';
import { deviceInfo, pasteboard } from '@kit.BasicServicesKit';
import { backgroundTaskManager } from '@kit.BackgroundTasksKit';
import { i18n } from '@kit.LocalizationKit';
import { display, window } from '@kit.ArkUI';
import { fileIo, fileUri, picker } from '@kit.CoreFileKit';
import {
  AbilityAware,
  AbilityPluginBinding,
  FlutterPlugin,
  FlutterPluginBinding,
  MethodCall,
  MethodCallHandler,
  MethodChannel,
  MethodResult,
} from '@ohos/flutter_ohos';
import {
  RustDeskClipboardController,
  RustDeskRemoteClipboardPayload,
} from './RustDeskClipboardController';
import { RustDeskSessionLaunch, RustDeskSessionWindows } from './RustDeskSessionWindows';

const METHOD_CHANNEL_NAME = 'top.frankhan.resk.flutter/platform';

export default class RustDeskPlatformBridge implements FlutterPlugin, MethodCallHandler, AbilityAware {
  private channel: MethodChannel | null = null;
  private context: Context | null = null;
  private abilityContext: common.UIAbilityContext | null = null;
  private continuousTaskId: number | null = null;
  private continuousTaskStarting: boolean = false;
  private readonly systemPasteboard: pasteboard.SystemPasteboard = pasteboard.getSystemPasteboard();
  private clipboardChangeCount: number = -1;
  private clipboardController: RustDeskClipboardController | null = null;
  private clipboardSessionId: string = '';
  private sessionLaunch: RustDeskSessionLaunch | null;
  private sessionWindowId: string;
  private sessionWindowTitle: string;
  private launchDelivered: boolean = false;

  constructor(launch: RustDeskSessionLaunch | null = null) {
    this.sessionLaunch = launch;
    this.sessionWindowId = launch?.windowId ?? '';
    this.sessionWindowTitle = launch?.title ?? '';
  }

  /// Resolves true only when the session may be torn down. A pending launch is
  /// released here because the window can be closed before Dart ever starts.
  requestWindowClose(): Promise<boolean> {
    if (!this.launchDelivered) {
      this.sessionLaunch = null;
      return Promise.resolve(true);
    }
    const channel = this.channel;
    if (channel === null) return Promise.resolve(false);
    return new Promise<boolean>((resolve: (value: boolean) => void): void => {
      channel.invokeMethod('onWindowClose', null, {
        success: (value: Object | null): void => {
          resolve(value === true);
        },
        error: (code: string): void => {
          console.error(`RustDesk session close request failed: ${code}`);
          resolve(false);
        },
        notImplemented: (): void => {
          resolve(false);
        },
      });
    });
  }

  getUniqueClassName(): string {
    return 'RustDeskPlatformBridge';
  }

  private async getDownloadDirectory(): Promise<string> {
    const context = this.abilityContext;
    if (context === null) {
      throw new Error('HarmonyOS UIAbility context is unavailable');
    }
    const options = new picker.DocumentSaveOptions();
    options.pickerMode = picker.DocumentPickerMode.DOWNLOAD;
    const uris = await new picker.DocumentViewPicker(context).save(options);
    if (uris.length === 0) {
      throw new Error('Application Download directory was not granted');
    }
    const path = new fileUri.FileUri(uris[0]).path;
    if (path.trim().length === 0) {
      throw new Error('Application Download directory has no filesystem path');
    }
    fileIo.listFileSync(path);
    return path;
  }

  onAttachedToEngine(binding: FlutterPluginBinding): void {
    this.context = binding.getApplicationContext();
    this.channel = new MethodChannel(binding.getBinaryMessenger(), METHOD_CHANNEL_NAME);
    this.channel.setMethodCallHandler(this);
  }

  onDetachedFromEngine(binding: FlutterPluginBinding): void {
    this.sessionLaunch = null;
    this.channel?.setMethodCallHandler(null);
    this.channel = null;
    this.context = null;
    this.disposeClipboardController();
  }

  onAttachedToAbility(binding: AbilityPluginBinding): void {
    this.abilityContext = binding.getAbility().context;
  }

  onDetachedFromAbility(): void {
    // Keep session roots across Ability recreation without retaining its context.
    this.clipboardController?.stop();
    // An explicitly started host survives Ability recreation, so its continuous-task indicator
    // must remain active until the local Stop service action tears both down.
    this.abilityContext = null;
  }

  onMethodCall(call: MethodCall, result: MethodResult): void {
    try {
      switch (call.method) {
        case 'getWindowLaunchPayload':
          if (this.sessionWindowId.length === 0) {
            result.success(null);
          } else {
            this.launchDelivered = true;
            const launch = this.sessionLaunch;
            this.sessionLaunch = null;
            result.success({
              'windowId': this.sessionWindowId,
              'payload': launch?.payload ?? '',
            });
          }
          return;
        case 'openSessionWindow':
          const payload = (call.argument('payload') as string | null) ?? '';
          const title = (call.argument('title') as string | null) ?? '';
          if (this.abilityContext === null || payload.length === 0) {
            result.error('OHOS_WINDOW_UNAVAILABLE', 'A window context and session payload are required', null);
            return;
          }
          RustDeskSessionWindows.open(this.abilityContext, payload, title)
            .then((windowId: string) => result.success(windowId))
            .catch(() => result.error('OHOS_WINDOW_START_FAILED', 'Unable to open a session window', null));
          return;
        case 'getFilesDir':
          result.success(this.abilityContext?.filesDir ?? this.context?.filesDir ?? '');
          return;
        case 'getDownloadDirectory':
          this.getDownloadDirectory()
            .then((path: string) => result.success(path))
            .catch((error: Error) =>
              result.error('OHOS_DOWNLOAD_DIRECTORY_ERROR', this.errorMessage(error), null));
          return;
        case 'getDeviceType':
          result.success(deviceInfo.deviceType);
          return;
        case 'getSystemLocale':
          result.success(i18n.System.getSystemLocale());
          return;
        case 'getDefaultDisplayInfo':
          const displayInfo = display.getDefaultDisplaySync();
          result.success({
            'displayId': displayInfo.id,
            'width': displayInfo.width,
            'height': displayInfo.height,
          });
          return;
        case 'hasClipboardPermission':
          result.success(this.hasClipboardPermission());
          return;
        case 'requestClipboardPermission':
          this.requestClipboardPermission(result);
          return;
        case 'getClipboardText':
          this.getClipboardText(call, result);
          return;
        case 'setClipboardText':
          this.setClipboardText(call, result);
          return;
        case 'prepareClipboardSession':
          this.prepareClipboardSession(call, result);
          return;
        case 'setClipboardSession':
          this.setClipboardSession(call, result);
          return;
        case 'applyClipboardData':
          this.applyClipboardData(call, result);
          return;
        case 'closeClipboardSession':
          this.closeClipboardSession(call, result);
          return;
        case 'getAudioInputDevices':
          result.success(this.getAudioDevices(audio.DeviceFlag.INPUT_DEVICES_FLAG));
          return;
        case 'getAudioOutputDevices':
          result.success(this.getAudioDevices(audio.DeviceFlag.OUTPUT_DEVICES_FLAG));
          return;
        case 'selectAudioInputDevice':
          this.selectAudioInputDevice(call, result);
          return;
        case 'prepareWindow':
          this.withMainWindow(result, async (mainWindow: window.Window): Promise<Object | null> => {
            if (this.sessionWindowId.length > 0) {
              // The session window draws its own title bar, exactly like the main window.
              try {
                await this.hideWindowCaption(mainWindow);
                await mainWindow.setWindowTitle(this.sessionWindowTitle);
                console.info(
                  `RustDesk session window prepared, decorHeight=${mainWindow.getWindowDecorHeight()}`);
              } catch (error) {
                console.error(
                  `RustDesk session window preparation failed: ${String(error)}`);
              }
              return null;
            }
            try {
              await this.hideWindowCaption(mainWindow);
              console.info(
                `RustDesk main window prepared, decorHeight=${mainWindow.getWindowDecorHeight()}`);
            } catch (error) {
              console.error(`RustDesk main window preparation failed: ${String(error)}`);
            }
            return null;
          });
          return;
        case 'getWindowStatus':
          this.withMainWindow(result, async (mainWindow: window.Window): Promise<Object | null> =>
            this.windowStatusName(mainWindow));
          return;
        case 'watchWindowStatus':
          this.withMainWindow(result, async (mainWindow: window.Window): Promise<Object | null> => {
            this.observeWindowMode(mainWindow);
            return this.windowStatusName(mainWindow);
          });
          return;
        case 'isWindowMaximized':
          this.withMainWindow(result, async (mainWindow: window.Window): Promise<Object | null> =>
            this.isMaximized(mainWindow));
          return;
        case 'minimizeWindow':
          this.withMainWindow(result, async (mainWindow: window.Window): Promise<Object | null> => {
            await mainWindow.minimize();
            return null;
          });
          return;
        case 'toggleMaximizeWindow':
          this.withMainWindow(result, async (mainWindow: window.Window): Promise<Object | null> => {
            if (this.isMaximized(mainWindow)) {
              await mainWindow.recover();
              return false;
            }
            await mainWindow.maximize(window.MaximizePresentation.EXIT_IMMERSIVE);
            return true;
          });
          return;
        case 'startMovingWindow':
          this.withMainWindow(result, async (mainWindow: window.Window): Promise<Object | null> => {
            await mainWindow.startMoving();
            return null;
          });
          return;
        case 'setKeepScreenOn':
          this.withMainWindow(result, async (mainWindow: window.Window): Promise<Object | null> => {
            const enabled = (call.argument('enabled') as boolean | null) ?? false;
            await mainWindow.setWindowKeepScreenOn(enabled);
            return null;
          });
          return;
        case 'setFullscreen':
          this.withMainWindow(result, async (mainWindow: window.Window): Promise<Object | null> => {
            const enabled = (call.argument('enabled') as boolean | null) ?? false;
            this.immersiveRequested = enabled;
            if (enabled) {
              await this.enterImmersive(mainWindow);
            } else {
              // Never bring the caption back: the app draws its own title bar and a re-shown
              // caption would sit above it after leaving fullscreen.
              await mainWindow.setWindowLayoutFullScreen(false);
              await this.setSystemBarsVisible(mainWindow, true);
              if (this.isMaximized(mainWindow)) {
                await mainWindow.recover();
              }
            }
            return null;
          });
          return;
        case 'startContinuousTask':
          this.startContinuousTask(result);
          return;
        case 'stopContinuousTask':
          this.stopContinuousTask(result);
          return;
        case 'closeWindow':
          if (this.abilityContext === null) {
            result.error('OHOS_WINDOW_CONTEXT_MISSING', 'HarmonyOS UIAbility context is unavailable', null);
            return;
          }
          this.abilityContext.terminateSelf()
            .then(() => result.success(null))
            .catch((error: Error) => result.error('OHOS_WINDOW_ERROR', this.errorMessage(error), null));
          return;
        default:
          result.notImplemented();
      }
    } catch (error) {
      result.error('OHOS_PLATFORM_ERROR', this.errorMessage(error), null);
    }
  }

  private withMainWindow(
    result: MethodResult,
    action: (mainWindow: window.Window) => Promise<Object | null>,
  ): void {
    const context = this.abilityContext;
    if (context === null) {
      result.error('OHOS_WINDOW_CONTEXT_MISSING', 'HarmonyOS application context is unavailable', null);
      return;
    }
    Promise.resolve().then(() => action(context.windowStage.getMainWindowSync()))
      .then((value: Object | null) => result.success(value))
      .catch((error: Error) => result.error('OHOS_WINDOW_ERROR', this.errorMessage(error), null));
  }

  private hasClipboardPermission(): boolean {
    const context = this.abilityContext ?? this.context;
    if (context === null) {
      return false;
    }
    return abilityAccessCtrl.createAtManager().checkAccessTokenSync(
      context.applicationInfo.accessTokenId, 'ohos.permission.READ_PASTEBOARD'
    ) === abilityAccessCtrl.GrantStatus.PERMISSION_GRANTED;
  }

  private requestClipboardPermission(result: MethodResult): void {
    if (this.hasClipboardPermission()) {
      result.success(true);
      return;
    }
    if (this.abilityContext === null) {
      result.error('OHOS_CLIPBOARD_CONTEXT_MISSING', 'HarmonyOS UIAbility context is unavailable', null);
      return;
    }
    const permission: Permissions = 'ohos.permission.READ_PASTEBOARD';
    abilityAccessCtrl.createAtManager()
      .requestPermissionsFromUser(this.abilityContext, [permission])
      .then(() => result.success(this.hasClipboardPermission()))
      .catch((error: Error) => result.error('OHOS_CLIPBOARD_PERMISSION_ERROR', this.errorMessage(error), null));
  }

  private getClipboardText(call: MethodCall, result: MethodResult): void {
    if (!this.hasClipboardPermission()) {
      result.error('OHOS_CLIPBOARD_PERMISSION_DENIED', 'Clipboard access is not authorized', null);
      return;
    }
    const changeCount = this.systemPasteboard.getChangeCount();
    const force = (call.argument('force') as boolean | null) ?? false;
    if (!force && changeCount === this.clipboardChangeCount) {
      result.success(null);
      return;
    }
    if (!this.systemPasteboard.hasDataType(pasteboard.MIMETYPE_TEXT_PLAIN)) {
      this.clipboardChangeCount = changeCount;
      result.success(null);
      return;
    }
    this.systemPasteboard.getData()
      .then((data: pasteboard.PasteData) => {
        this.clipboardChangeCount = changeCount;
        result.success(data.getPrimaryText());
      })
      .catch((error: Error) => result.error('OHOS_CLIPBOARD_READ_ERROR', this.errorMessage(error), null));
  }

  private setClipboardText(call: MethodCall, result: MethodResult): void {
    const text = (call.argument('text') as string | null) ?? '';
    this.systemPasteboard.setData(pasteboard.createPlainTextData(text))
      .then(() => result.success(null))
      .catch((error: Error) => result.error('OHOS_CLIPBOARD_WRITE_ERROR', this.errorMessage(error), null));
  }

  private ensureClipboardController(): RustDeskClipboardController | null {
    const context = this.abilityContext;
    if (context === null) {
      return null;
    }
    if (this.clipboardController !== null) {
      return this.clipboardController;
    }
    this.clipboardController = new RustDeskClipboardController(context, {
      sendText: (sessionId: string, content: string): Promise<boolean> =>
        this.sendClipboardToDart('onClipboardText', sessionId, 'text', content),
      sendHtml: (sessionId: string, html: string, text: string): Promise<boolean> =>
        this.sendClipboardHtmlToDart(sessionId, html, text),
      sendImage: (sessionId: string, bytes: Uint8Array): Promise<boolean> =>
        this.sendClipboardToDart('onClipboardImage', sessionId, 'image', bytes),
      sendFiles: (sessionId: string, paths: Array<string>): Promise<boolean> =>
        this.sendClipboardToDart('onClipboardFiles', sessionId, 'paths', paths),
      log: (label: string, payload: string): void => {
        console.info(`RustDesk clipboard ${label}: ${payload}`);
      },
    });
    return this.clipboardController;
  }

  private disposeClipboardController(): void {
    const controller = this.clipboardController;
    this.clipboardController = null;
    this.clipboardSessionId = '';
    if (controller !== null) {
      controller.reset();
    }
  }

  private prepareClipboardSession(call: MethodCall, result: MethodResult): void {
    const sessionId = (call.argument('sessionId') as string | null) ?? '';
    if (sessionId.length === 0) {
      result.error('OHOS_CLIPBOARD_SESSION_INVALID', 'A clipboard session id is required', null);
      return;
    }
    const controller = this.ensureClipboardController();
    if (controller === null) {
      result.error('OHOS_CLIPBOARD_CONTEXT_MISSING', 'HarmonyOS UIAbility context is unavailable', null);
      return;
    }
    // Preparing a root only publishes the staging directory; watching starts in setClipboardSession.
    result.success(controller.incomingFileRoot(sessionId));
  }

  private setClipboardSession(call: MethodCall, result: MethodResult): void {
    const sessionId = (call.argument('sessionId') as string | null) ?? '';
    if (sessionId.length === 0) {
      this.clipboardSessionId = '';
      const controller = this.clipboardController;
      if (controller === null) {
        result.success(false);
        return;
      }
      controller.stopAndDrainRemoteWrite()
        .then(() => result.success(false))
        .catch((error: Error) =>
          result.error('OHOS_CLIPBOARD_SESSION_ERROR', this.errorMessage(error), null));
      return;
    }
    const controller = this.ensureClipboardController();
    if (controller === null) {
      result.success(false);
      return;
    }
    this.clipboardSessionId = sessionId;
    controller.start(sessionId);
    result.success(true);
  }

  private applyClipboardData(call: MethodCall, result: MethodResult): void {
    const sessionId = (call.argument('sessionId') as string | null) ?? '';
    if (sessionId.length === 0) {
      result.error('OHOS_CLIPBOARD_SESSION_INVALID', 'A clipboard session id is required', null);
      return;
    }
    const controller = this.ensureClipboardController();
    if (controller === null) {
      result.error('OHOS_CLIPBOARD_CONTEXT_MISSING', 'HarmonyOS UIAbility context is unavailable', null);
      return;
    }
    const payload: RustDeskRemoteClipboardPayload = {};
    const text = (call.argument('text') as string | null) ?? null;
    if (text !== null) {
      payload.text = text;
    }
    const html = (call.argument('html') as string | null) ?? null;
    if (html !== null) {
      payload.html = html;
    }
    const files = (call.argument('files') as Array<string> | null) ?? null;
    if (files !== null && files.length > 0) {
      payload.files = files;
    }
    const imageBytes = (call.argument('image') as Uint8Array | null) ?? null;
    if (imageBytes !== null && imageBytes.length > 0) {
      payload.image = {
        format: (call.argument('imageFormat') as string | null) ?? 'png',
        width: (call.argument('width') as number | null) ?? 0,
        height: (call.argument('height') as number | null) ?? 0,
        bytes: imageBytes,
      };
    }
    controller.applyRemoteClipboard(sessionId, payload)
      .then(() => result.success(null))
      .catch((error: Error) => result.error('OHOS_CLIPBOARD_WRITE_ERROR', this.errorMessage(error), null));
  }

  private closeClipboardSession(call: MethodCall, result: MethodResult): void {
    const sessionId = (call.argument('sessionId') as string | null) ?? '';
    const controller = this.clipboardController;
    if (controller === null || sessionId.length === 0) {
      result.success(null);
      return;
    }
    this.closeClipboardSessionInternal(sessionId, controller)
      .then(() => result.success(null))
      .catch((error: Error) =>
        result.error('OHOS_CLIPBOARD_SESSION_ERROR', this.errorMessage(error), null));
  }

  private async closeClipboardSessionInternal(
    sessionId: string,
    controller: RustDeskClipboardController,
  ): Promise<void> {
    if (sessionId === this.clipboardSessionId) {
      this.clipboardSessionId = '';
      await controller.stopAndDrainRemoteWrite();
    }
    await controller.cleanupIncomingRootForClosedSession(sessionId);
  }

  private sendClipboardToDart(
    method: string,
    sessionId: string,
    argumentName: string,
    argumentValue: string | Uint8Array | Array<string>,
  ): Promise<boolean> {
    if (sessionId !== this.clipboardSessionId) {
      return Promise.resolve(false);
    }
    const args = new Map<string, string | Uint8Array | Array<string>>();
    args.set('sessionId', sessionId);
    args.set(argumentName, argumentValue);
    return this.invokeDartBoolean(method, args);
  }

  private sendClipboardHtmlToDart(sessionId: string, html: string, text: string): Promise<boolean> {
    if (sessionId !== this.clipboardSessionId) {
      return Promise.resolve(false);
    }
    const args = new Map<string, string | Uint8Array | Array<string>>();
    args.set('sessionId', sessionId);
    args.set('html', html);
    args.set('text', text);
    return this.invokeDartBoolean('onClipboardHtml', args);
  }

  private invokeDartBoolean(method: string, args: Map<string, string | Uint8Array | Array<string>>): Promise<boolean> {
    const channel = this.channel;
    if (channel === null) {
      return Promise.resolve(false);
    }
    return new Promise<boolean>((resolve: (value: boolean) => void): void => {
      channel.invokeMethod(method, args, {
        success: (value: Object | null): void => {
          resolve(value === true);
        },
        error: (code: string, message: string): void => {
          console.error(`RustDesk clipboard ${method} was rejected: ${code} ${message}`);
          resolve(false);
        },
        notImplemented: (): void => {
          resolve(false);
        },
      });
    });
  }

  private startContinuousTask(result: MethodResult): void {
    if (this.abilityContext === null) {
      result.error('OHOS_BACKGROUND_CONTEXT_MISSING', 'HarmonyOS UIAbility context is unavailable', null);
      return;
    }
    if (this.continuousTaskId !== null || this.continuousTaskStarting) {
      result.success(null);
      return;
    }
    this.continuousTaskStarting = true;
    const context = this.abilityContext;
    const wantAgentInfo: wantAgent.WantAgentInfo = {
      wants: [{
        bundleName: 'top.frankhan.resk.flutter',
        abilityName: 'EntryAbility',
      }],
      actionType: wantAgent.OperationType.START_ABILITY,
      requestCode: 0,
      wantAgentFlags: [wantAgent.WantAgentFlags.UPDATE_PRESENT_FLAG],
    };
    wantAgent.getWantAgent(wantAgentInfo)
      .then((agent: WantAgent) => backgroundTaskManager.startBackgroundRunning(
        context,
        ['dataTransfer', 'audioRecording'],
        agent,
      ))
      .then((task: backgroundTaskManager.ContinuousTaskNotification) => {
        this.continuousTaskId = task.continuousTaskId ?? null;
        this.continuousTaskStarting = false;
        result.success(null);
      })
      .catch((error: Error) => {
        this.continuousTaskStarting = false;
        result.error('OHOS_BACKGROUND_START_ERROR', this.errorMessage(error), null);
      });
  }

  private stopContinuousTask(result: MethodResult): void {
    if (this.abilityContext === null) {
      result.error('OHOS_BACKGROUND_CONTEXT_MISSING', 'HarmonyOS UIAbility context is unavailable', null);
      return;
    }
    this.stopContinuousTaskInternal(this.abilityContext)
      .then(() => result.success(null))
      .catch((error: Error) =>
        result.error('OHOS_BACKGROUND_STOP_ERROR', this.errorMessage(error), null));
  }

  private async stopContinuousTaskInternal(context: common.UIAbilityContext): Promise<void> {
    const taskId = this.continuousTaskId;
    this.continuousTaskId = null;
    this.continuousTaskStarting = false;
    if (taskId === null) {
      return;
    }
    await backgroundTaskManager.stopBackgroundRunning(context, taskId);
  }

  /// Asks the system to drop the window caption so the app can draw its own title bar.
  ///
  /// Measured on a 2in1 emulator and on a tablet: the caption stays at 37px no matter what
  /// this calls, so the app keeps the platform caption and must not maximize the window to
  /// try to get rid of it (that would also leave the free-form layout).
  private async hideWindowCaption(mainWindow: window.Window): Promise<void> {
    await mainWindow.setWindowDecorVisible(false);
    mainWindow.setWindowTitleButtonVisible(false, false, false);
    mainWindow.setWindowTitleMoveEnabled(false);
  }

  private isMaximized(mainWindow: window.Window): boolean {
    const status = mainWindow.getWindowStatus();
    return status === window.WindowStatusType.MAXIMIZE || status === window.WindowStatusType.FULL_SCREEN;
  }

  private async setSystemBarsVisible(mainWindow: window.Window, visible: boolean): Promise<void> {
    try {
      await mainWindow.setSpecificSystemBarEnabled('status', visible);
      await mainWindow.setSpecificSystemBarEnabled('navigationIndicator', visible);
    } catch (error) {
      // Freeform and auxiliary windows cannot hide the system bars, so the session keeps
      // whatever the desktop shell shows.
      console.error(`RustDesk system bar toggle failed: ${String(error)}`);
    }
  }

  /// Fullscreen on a 2in1 also has to hide the desktop dock: only the immersive
  /// presentation does that, and the shell only keeps it while the window is maximized.
  private async enterImmersive(mainWindow: window.Window): Promise<void> {
    await mainWindow.maximize(window.MaximizePresentation.ENTER_IMMERSIVE);
    await mainWindow.setWindowLayoutFullScreen(true);
    mainWindow.setWindowDecorVisible(false);
    await this.setSystemBarsVisible(mainWindow, false);
    this.observeWindowStatus(mainWindow);
  }

  private observeWindowStatus(mainWindow: window.Window): void {
    if (this.immersiveObserved) {
      return;
    }
    this.immersiveObserved = true;
    try {
      mainWindow.on('windowStatusChange', (status: window.WindowStatusType) => {
        if (status === window.WindowStatusType.MAXIMIZE && this.immersiveRequested) {
          mainWindow.maximize(window.MaximizePresentation.ENTER_IMMERSIVE)
            .catch((error: Error) => console.error(
              `RustDesk immersive re-entry failed: ${this.errorMessage(error)}`));
        }
      });
    } catch (error) {
      console.error(`RustDesk window status listener failed: ${String(error)}`);
    }
  }

  private immersiveRequested: boolean = false;
  private immersiveObserved: boolean = false;

  private windowStatusName(mainWindow: window.Window): string {
    const status = mainWindow.getWindowStatus();
    if (status === window.WindowStatusType.FLOATING) {
      return 'floating';
    }
    if (status === window.WindowStatusType.SPLIT_SCREEN) {
      return 'split';
    }
    if (status === window.WindowStatusType.FULL_SCREEN) {
      return 'fullscreen';
    }
    if (status === window.WindowStatusType.MAXIMIZE) {
      return 'maximize';
    }
    return 'recover';
  }

  /// Report window mode changes so the frontend can switch between the mobile and the
  /// desktop layout, and keep the 2in1 minimum size in place.
  private observeWindowMode(mainWindow: window.Window): void {
    if (this.windowModeObserved) {
      return;
    }
    this.windowModeObserved = true;
    try {
      mainWindow.on('windowStatusChange', (status: window.WindowStatusType) => {
        this.channel?.invokeMethod('onWindowStatus', {
          'status': this.windowStatusName(mainWindow),
        });
        // The caption APIs settle with the window state, so measure after the change.
        console.info(
          `RustDesk window status changed, decorHeight=${mainWindow.getWindowDecorHeight()}`);
      });
    } catch (error) {
      console.error(`RustDesk window mode listener failed: ${String(error)}`);
    }
  }

  private windowModeObserved: boolean = false;

  private getAudioDevices(flag: audio.DeviceFlag): Array<string> {
    const routingManager = audio.getAudioManager().getRoutingManager();
    const descriptors = routingManager.getDevicesSync(flag);
    const names: Array<string> = [];
    descriptors.forEach((descriptor: Readonly<audio.AudioDeviceDescriptor>) => {
      const name = this.audioDeviceName(descriptor);
      if (!names.includes(name)) {
        names.push(name);
      }
    });
    return names;
  }

  private selectAudioInputDevice(call: MethodCall, result: MethodResult): void {
    const requested = call.argument('device') as string | null;
    const deviceName = requested ?? '';
    const audioManager = audio.getAudioManager();
    const sessionManager = audioManager.getSessionManager();
    if (deviceName.length === 0) {
      sessionManager.clearSelectedMediaInputDevice()
        .then(() => result.success(null))
        .catch((error: Error) => result.error('OHOS_AUDIO_ERROR', this.errorMessage(error), null));
      return;
    }
    const descriptors = audioManager.getRoutingManager().getDevicesSync(audio.DeviceFlag.INPUT_DEVICES_FLAG);
    const selected = descriptors.find((descriptor: Readonly<audio.AudioDeviceDescriptor>) =>
      this.audioDeviceName(descriptor) === deviceName);
    if (selected === undefined) {
      result.error('OHOS_AUDIO_DEVICE_NOT_FOUND', `Audio input device is unavailable: ${deviceName}`, null);
      return;
    }
    sessionManager.selectMediaInputDevice(selected)
      .then(() => result.success(null))
      .catch((error: Error) => result.error('OHOS_AUDIO_ERROR', this.errorMessage(error), null));
  }

  private audioDeviceName(descriptor: Readonly<audio.AudioDeviceDescriptor>): string {
    if (descriptor.displayName.length > 0) {
      return descriptor.displayName;
    }
    if (descriptor.name.length > 0) {
      return descriptor.name;
    }
    return `Audio device ${descriptor.id}`;
  }

  private errorMessage(error: Object): string {
    return error instanceof Error ? error.message : `${error}`;
  }
}
