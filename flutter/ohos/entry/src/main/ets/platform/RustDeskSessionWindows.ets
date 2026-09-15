import { common, Want } from '@kit.AbilityKit';

export class RustDeskSessionLaunch {
  windowId: string;
  payload: string;
  title: string;

  constructor(windowId: string, payload: string, title: string) {
    this.windowId = windowId;
    this.payload = payload;
    this.title = title;
  }
}

export class RustDeskSessionWindows {
  private static nextId: number = 0;
  private static pending: Map<string, RustDeskSessionLaunch> = new Map();

  static async open(context: common.UIAbilityContext, payload: string, title: string): Promise<string> {
    const windowId = `${Date.now()}-${++RustDeskSessionWindows.nextId}`;
    RustDeskSessionWindows.pending.set(windowId, new RustDeskSessionLaunch(windowId, payload, title));
    const want: Want = {
      bundleName: context.abilityInfo.bundleName,
      moduleName: context.abilityInfo.moduleName,
      abilityName: 'SessionAbility',
      parameters: { rustdeskSessionWindowId: windowId },
    };
    try {
      await context.startAbility(want);
      return windowId;
    } catch (error) {
      RustDeskSessionWindows.pending.delete(windowId);
      throw new Error(`Unable to start a RustDesk session window: ${error}`);
    }
  }

  static take(windowId: string): RustDeskSessionLaunch | undefined {
    const launch = RustDeskSessionWindows.pending.get(windowId);
    RustDeskSessionWindows.pending.delete(windowId);
    return launch;
  }
}
