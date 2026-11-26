import type { ConfigContext, ExpoConfig } from "expo/config";

export const GITPULSE_IOS_APP_GROUP = "group.com.gitpulse.mobile";
export const GITPULSE_BUNDLE_ID = "com.gitpulse.mobile";
export const GITPULSE_NSE_BUNDLE_ID = "com.gitpulse.mobile.NotificationService";
export const GITPULSE_ANDROID_PACKAGE = "com.gitpulse.mobile";
export const GITPULSE_CODE_SIGNING_CERT_PATH = "./certificates/code-signing-certificate.pem";

export interface GitPulseUpdatesConfig {
  url: string;
  enabled: boolean;
  fallbackToCacheTimeout: number;
  codeSigningCertificate: string;
  codeSigningMetadata: {
    keyid: string;
    alg: "rsa-v1_5-sha256";
  };
}

export interface GitPulseMobileConfig extends ExpoConfig {
  newArchEnabled: boolean;
  updates: GitPulseUpdatesConfig;
}

export function buildMobileAppConfig(context: Partial<ConfigContext> = {}): GitPulseMobileConfig {
  const base = context.config ?? {};
  const updatesUrl =
    process.env.GITPULSE_UPDATES_URL ?? "https://u.expo.dev/00000000-0000-0000-0000-000000000000";
  const codeSigningCert =
    process.env.GITPULSE_CODE_SIGNING_CERT ?? GITPULSE_CODE_SIGNING_CERT_PATH;

  const config: GitPulseMobileConfig = {
    ...base,
    name: "GitPulse",
    slug: "gitpulse-mobile",
    version: "0.1.0",
    orientation: "portrait",
    scheme: "gitpulse",
    userInterfaceStyle: "dark",
    // React Native New Architecture (Fabric + TurboModules) mandatory for JSI/Expo Modules
    newArchEnabled: true,
    runtimeVersion: {
      policy: "appVersion",
    },
    // Opus M3: Mandatory offline-key code-signed OTA updates.
    // Security-critical approval sheet & P-256 hardware signing remain strictly native/Rust.
    updates: {
      url: updatesUrl,
      enabled: true,
      fallbackToCacheTimeout: 0,
      codeSigningCertificate: codeSigningCert,
      codeSigningMetadata: {
        keyid: process.env.GITPULSE_CODE_SIGNING_KEYID ?? "main",
        alg: "rsa-v1_5-sha256",
      },
    },
    ios: {
      supportsTablet: true,
      bundleIdentifier: GITPULSE_BUNDLE_ID,
      entitlements: {
        "com.apple.security.application-groups": [GITPULSE_IOS_APP_GROUP],
        "aps-environment": "production",
        "keychain-access-groups": [`$(AppIdentifierPrefix)${GITPULSE_IOS_APP_GROUP}`],
      },
      infoPlist: {
        NSFaceIDUsageDescription:
          "GitPulse uses Face ID to authorize hardware-signed P-256 tool approval decisions and step-up terminal input.",
        NSCameraUsageDescription:
          "GitPulse uses the camera to scan desktop QR codes for end-to-end encrypted device pairing.",
        UIBackgroundModes: ["remote-notification"],
        ITSAppUsesNonExemptEncryption: false,
      },
    },
    android: {
      package: GITPULSE_ANDROID_PACKAGE,
      // Opus M5: Explicitly request biometric & notification permissions; NEVER USE_FULL_SCREEN_INTENT.
      permissions: [
        "android.permission.USE_BIOMETRIC",
        "android.permission.POST_NOTIFICATIONS",
        "android.permission.CAMERA",
        "android.permission.INTERNET",
        "android.permission.WAKE_LOCK",
      ],
      blockedPermissions: ["android.permission.USE_FULL_SCREEN_INTENT"],
    },
    plugins: [
      "expo-router",
      [
        "expo-updates",
        {
          username: "gitpulse",
        },
      ],
      [
        "expo-notifications",
        {
          color: "#0F172A",
          defaultChannel: "gitpulse_approvals",
        },
      ],
      [
        "expo-camera",
        {
          cameraPermission:
            "GitPulse uses the camera to scan desktop QR pairing codes.",
        },
      ],
      [
        "./plugins/with-nse",
        {
          appGroup: GITPULSE_IOS_APP_GROUP,
          nseBundleIdentifier: GITPULSE_NSE_BUNDLE_ID,
          androidPackage: GITPULSE_ANDROID_PACKAGE,
        },
      ],
    ],
    experiments: {
      typedRoutes: true,
    },
    extra: {
      appGroup: GITPULSE_IOS_APP_GROUP,
      otaSecurityScope: "non-security-ui-only",
    },
  };

  validateMobileAppConfig(config);
  return config;
}

/**
 * Fails closed if mandatory Opus M3/M5 security invariants are missing from app.config.ts.
 */
export function validateMobileAppConfig(config: Partial<GitPulseMobileConfig>): void {
  if (config.newArchEnabled !== true) {
    throw new Error("SECURITY VIOLATION: React Native New Architecture (newArchEnabled: true) is mandatory.");
  }

  const updates = config.updates;
  if (!updates?.codeSigningCertificate || updates.codeSigningCertificate.trim().length === 0) {
    throw new Error(
      "SECURITY VIOLATION (Opus M3): expo-updates codeSigningCertificate is mandatory.",
    );
  }

  const certLower = updates.codeSigningCertificate.toLowerCase();
  if (certLower.includes("private") || certLower.endsWith(".key")) {
    throw new Error(
      "SECURITY VIOLATION (Opus M3): Private signing keys MUST NEVER be referenced in app.config.ts.",
    );
  }

  if (
    !updates.codeSigningMetadata ||
    !updates.codeSigningMetadata.keyid ||
    updates.codeSigningMetadata.alg !== "rsa-v1_5-sha256"
  ) {
    throw new Error(
      "SECURITY VIOLATION (Opus M3): expo-updates codeSigningMetadata must specify keyid and alg 'rsa-v1_5-sha256'.",
    );
  }

  const iosGroups = (
    config.ios?.entitlements as Record<string, unknown> | undefined
  )?.["com.apple.security.application-groups"];
  if (!Array.isArray(iosGroups) || !iosGroups.includes(GITPULSE_IOS_APP_GROUP)) {
    throw new Error(
      `SECURITY VIOLATION (Opus M2): iOS entitlements must include App Group ${GITPULSE_IOS_APP_GROUP}.`,
    );
  }

  const androidPerms = config.android?.permissions ?? [];
  if (androidPerms.includes("android.permission.USE_FULL_SCREEN_INTENT")) {
    throw new Error(
      "SECURITY VIOLATION (Opus M5): Android USE_FULL_SCREEN_INTENT is forbidden.",
    );
  }
}

export default function expoConfig(context: ConfigContext): GitPulseMobileConfig {
  return buildMobileAppConfig(context);
}
