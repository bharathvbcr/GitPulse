import fs from "node:fs";
import path from "node:path";
import type { ExpoConfig } from "expo/config";

/**
 * Opus M1 Binding Security Invariant:
 * Lock-screen and notification quick-actions MUST expose ONLY Deny and Deny & Stop.
 * NEVER register an Approve action on lock screen or notification categories.
 * Tapping the notification opens the app to the native full-bytes approval sheet.
 */
export const APPROVAL_NOTIFICATION_CATEGORY_ID = "GITPULSE_APPROVAL";
export const ACTION_DENY_ID = "GITPULSE_ACTION_DENY";
export const ACTION_DENY_AND_INTERRUPT_ID = "GITPULSE_ACTION_DENY_AND_INTERRUPT";
export const MAX_PREVIEW_CT_BYTES = 2048;

export interface NotificationActionSpec {
  readonly identifier: typeof ACTION_DENY_ID | typeof ACTION_DENY_AND_INTERRUPT_ID;
  readonly buttonTitle: "Deny" | "Deny & Stop";
  readonly options: {
    readonly opensAppToForeground: false;
    readonly isDestructive: true;
    readonly isAuthenticationRequired: true;
  };
}

export interface NotificationCategorySpec {
  readonly identifier: string;
  readonly actions: readonly NotificationActionSpec[];
  readonly allowApproveOnLockScreen: false;
}

export const APPROVAL_NOTIFICATION_ACTIONS: readonly NotificationActionSpec[] = Object.freeze([
  Object.freeze({
    identifier: ACTION_DENY_ID,
    buttonTitle: "Deny",
    options: Object.freeze({
      opensAppToForeground: false,
      isDestructive: true,
      isAuthenticationRequired: true,
    }),
  }),
  Object.freeze({
    identifier: ACTION_DENY_AND_INTERRUPT_ID,
    buttonTitle: "Deny & Stop",
    options: Object.freeze({
      opensAppToForeground: false,
      isDestructive: true,
      isAuthenticationRequired: true,
    }),
  }),
]);

export const NOTIFICATION_CATEGORIES: readonly NotificationCategorySpec[] = Object.freeze([
  Object.freeze({
    identifier: APPROVAL_NOTIFICATION_CATEGORY_ID,
    actions: APPROVAL_NOTIFICATION_ACTIONS,
    allowApproveOnLockScreen: false,
  }),
]);

/**
 * Runtime guard asserting Opus M1: zero Approve actions exist anywhere in notification categories.
 */
export function assertNoLockScreenApproveActions(
  categories: readonly { identifier: string; actions: readonly { identifier: string; buttonTitle: string }[] }[],
): void {
  for (const category of categories) {
    for (const action of category.actions) {
      const idLower = action.identifier.toLowerCase();
      const titleLower = action.buttonTitle.toLowerCase();
      if (
        idLower.includes("approve") ||
        idLower.includes("allow") ||
        titleLower.includes("approve") ||
        titleLower.includes("allow")
      ) {
        throw new Error(
          `SECURITY VIOLATION (Opus M1): Forbidden lock-screen approve action "${action.identifier}" ("${action.buttonTitle}") in category "${category.identifier}".`,
        );
      }
    }
  }
}

export interface WithNsePluginOptions {
  appGroup?: string;
  nseBundleIdentifier?: string;
  androidPackage?: string;
}

export interface ConfigPluginManifestResult {
  iosExtensionName: string;
  iosBundleIdentifier: string;
  iosAppGroup: string;
  iosUsesHermes: false;
  iosMemoryBudgetMb: 24;
  androidMessagingServiceClass: string;
  androidActionReceiverClass: string;
  androidBudgetSeconds: 10;
  androidAlwaysPostsVisibleNotification: true;
  notificationCategories: readonly NotificationCategorySpec[];
}

export function describePushExtensionsPlan(
  options: WithNsePluginOptions = {},
): ConfigPluginManifestResult {
  assertNoLockScreenApproveActions(NOTIFICATION_CATEGORIES);
  return {
    iosExtensionName: "GitPulseNotificationService",
    iosBundleIdentifier:
      options.nseBundleIdentifier ?? "com.gitpulse.mobile.NotificationService",
    iosAppGroup: options.appGroup ?? "group.com.gitpulse.mobile",
    iosUsesHermes: false,
    iosMemoryBudgetMb: 24,
    androidMessagingServiceClass: "com.gitpulse.mobile.push.GitPulseMessagingService",
    androidActionReceiverClass: "com.gitpulse.mobile.push.GitPulseNotificationActionReceiver",
    androidBudgetSeconds: 10,
    androidAlwaysPostsVisibleNotification: true,
    notificationCategories: NOTIFICATION_CATEGORIES,
  };
}

/**
 * Expo Config Plugin wiring:
 * 1. iOS Notification Service Extension (Swift -> Rust GitPulseProtocol.xcframework, NEVER Hermes).
 * 2. Android FirebaseMessagingService (Kotlin -> libgitpulse_protocol.so, 10s budget, always visible).
 * 3. Lock-screen Deny / Deny & Stop ONLY categories (Opus M1).
 */
export function withGitPulsePushExtensions<T extends ExpoConfig>(
  config: T,
  options: WithNsePluginOptions = {},
): T {
  const plan = describePushExtensionsPlan(options);

  // Ensure iOS App Group entitlements are set on main target
  const iosEntitlements = {
    ...(config.ios?.entitlements ?? {}),
    "com.apple.security.application-groups": [plan.iosAppGroup],
    "keychain-access-groups": [`$(AppIdentifierPrefix)${plan.iosAppGroup}`],
  };

  // Wire @expo/config-plugins dangerous/manifest mods lazily if installed during `expo prebuild`
  let nextConfig: ExpoConfig = {
    ...config,
    ios: {
      ...config.ios,
      entitlements: iosEntitlements,
    },
    extra: {
      ...(config.extra ?? {}),
      pushExtensions: plan,
    },
  };

  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const configPlugins = require("@expo/config-plugins") as {
      withDangerousMod?: (
        cfg: ExpoConfig,
        params: [string, (c: { modRequest: { projectRoot: string; platformProjectRoot: string } }) => Promise<unknown> | unknown],
      ) => ExpoConfig;
      withAndroidManifest?: (
        cfg: ExpoConfig,
        mod: (c: { modResults: { manifest: { application?: Array<Record<string, unknown>> } } }) => unknown,
      ) => ExpoConfig;
    };

    if (configPlugins.withDangerousMod) {
      // Copy Swift NSE sources + entitlements + Info.plist into ios/GitPulseNotificationService/
      nextConfig = configPlugins.withDangerousMod(nextConfig, [
        "ios",
        async (modConfig) => {
          const templateDir = path.resolve(__dirname, "templates/ios");
          const targetDir = path.join(
            modConfig.modRequest.platformProjectRoot,
            plan.iosExtensionName,
          );
          fs.mkdirSync(targetDir, { recursive: true });
          for (const filename of [
            "NotificationService.swift",
            "Info.plist",
            "GitPulseNotificationService.entitlements",
          ]) {
            const src = path.join(templateDir, filename);
            if (fs.existsSync(src)) {
              const content = fs
                .readFileSync(src, "utf8")
                .replaceAll("__APP_GROUP__", plan.iosAppGroup)
                .replaceAll("__NSE_BUNDLE_ID__", plan.iosBundleIdentifier);
              fs.writeFileSync(path.join(targetDir, filename), content, "utf8");
            }
          }
          return modConfig;
        },
      ]);

      // Copy Kotlin FirebaseMessagingService + BroadcastReceiver into android/app/src/main/java/...
      nextConfig = configPlugins.withDangerousMod(nextConfig, [
        "android",
        async (modConfig) => {
          const templateDir = path.resolve(__dirname, "templates/android");
          const pkgPath = (options.androidPackage ?? "com.gitpulse.mobile").replaceAll(".", "/");
          const targetDir = path.join(
            modConfig.modRequest.platformProjectRoot,
            "app/src/main/java",
            pkgPath,
            "push",
          );
          fs.mkdirSync(targetDir, { recursive: true });
          for (const filename of [
            "GitPulseMessagingService.kt",
            "GitPulseNotificationActionReceiver.kt",
          ]) {
            const src = path.join(templateDir, filename);
            if (fs.existsSync(src)) {
              fs.copyFileSync(src, path.join(targetDir, filename));
            }
          }
          return modConfig;
        },
      ]);
    }

    if (configPlugins.withAndroidManifest) {
      nextConfig = configPlugins.withAndroidManifest(nextConfig, (modConfig) => {
        const app = modConfig.modResults.manifest.application?.[0];
        if (app) {
          const services = (app.service as Array<Record<string, unknown>> | undefined) ?? [];
          const hasService = services.some(
            (s) =>
              (s.$ as Record<string, string> | undefined)?.["android:name"] ===
              ".push.GitPulseMessagingService",
          );
          if (!hasService) {
            services.push({
              $: {
                "android:name": ".push.GitPulseMessagingService",
                "android:exported": "false",
              },
              "intent-filter": [
                {
                  action: [{ $: { "android:name": "com.google.firebase.MESSAGING_EVENT" } }],
                },
              ],
            });
            app.service = services;
          }

          const receivers = (app.receiver as Array<Record<string, unknown>> | undefined) ?? [];
          const hasReceiver = receivers.some(
            (r) =>
              (r.$ as Record<string, string> | undefined)?.["android:name"] ===
              ".push.GitPulseNotificationActionReceiver",
          );
          if (!hasReceiver) {
            receivers.push({
              $: {
                "android:name": ".push.GitPulseNotificationActionReceiver",
                "android:exported": "false",
              },
            });
            app.receiver = receivers;
          }
        }
        return modConfig;
      });
    }
  } catch {
    // @expo/config-plugins is only required during `expo prebuild`; pure config inspection works without it.
  }

  return nextConfig as T;
}

export default withGitPulsePushExtensions;
