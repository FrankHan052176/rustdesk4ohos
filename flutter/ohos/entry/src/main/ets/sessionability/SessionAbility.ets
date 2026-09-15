import { AbilityConstant, Want } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { FlutterAbility, FlutterEngine } from '@ohos/flutter_ohos';
import { GeneratedPluginRegistrant } from '../plugins/GeneratedPluginRegistrant';
import RustDeskPlatformBridge from '../platform/RustDeskPlatformBridge';
import { RustDeskSessionLaunch, RustDeskSessionWindows } from '../platform/RustDeskSessionWindows';

export default class SessionAbility extends FlutterAbility {
  private platformBridge: RustDeskPlatformBridge | null = null;
  // resolve(true) keeps the window open, so only a completed Dart teardown may close it.
  private closeRequested = async (): Promise<boolean> => {
    const bridge = this.platformBridge;
    if (bridge === null) {
      return false;
    }
    return !(await bridge.requestWindowClose());
  };

  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    let windowId = '';
    const parameters = want.parameters;
    if (parameters !== undefined) {
      const requested = parameters['rustdeskSessionWindowId'] as string | undefined;
      if (requested !== undefined) {
        windowId = requested;
      }
    }
    this.platformBridge = new RustDeskPlatformBridge(
      RustDeskSessionWindows.take(windowId) ?? new RustDeskSessionLaunch(windowId, '', 'RustDesk'));
    super.onCreate(want, launchParam);
  }

  getDartEntrypointFunctionName(): string {
    return 'ohosSessionMain';
  }

  configureFlutterEngine(flutterEngine: FlutterEngine): void {
    super.configureFlutterEngine(flutterEngine);
    GeneratedPluginRegistrant.registerWith(flutterEngine);
    const bridge = this.platformBridge;
    if (bridge !== null) {
      flutterEngine.getPlugins()?.add(bridge);
    }
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    super.onWindowStageCreate(windowStage);
    try {
      windowStage.getMainWindowSync().on('windowWillClose', this.closeRequested);
    } catch (error) {
      console.error(`Failed to observe the RustDesk session window close: ${error}`);
    }
  }

  onWindowStageWillDestroy(windowStage: window.WindowStage): void {
    try {
      windowStage.getMainWindowSync().off('windowWillClose', this.closeRequested);
    } catch (error) {
      console.error(`Failed to stop observing the RustDesk session window close: ${error}`);
    }
    super.onWindowStageWillDestroy(windowStage);
  }
}
