import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import {
  GitPulseCrypto,
  type DevicePublicKeys,
  type NativeApprovalSheetOutcome,
  type NativeSteeringSheetOutcome,
} from "@gitpulse/crypto";
import {
  ControlPlaneClient,
  type ControlPlaneState,
  type WebSocketLike,
} from "../client/ControlPlaneClient";
import {
  type ApprovalTicket,
  type SessionSnapshot,
  type SteeringConfirmReq,
  type SteeringConfirmResp,
  type SteeringPreviewReq,
  type SteeringPreviewResp,
} from "../protocol/types";
import {
  ACTION_DENY_AND_INTERRUPT_ID,
  ACTION_DENY_ID,
  assertNoLockScreenApproveActions,
  NOTIFICATION_CATEGORIES,
} from "../../plugins/with-nse";

export interface ControlPlaneContextValue {
  readonly state: ControlPlaneState;
  readonly deviceKeys: DevicePublicKeys | null;
  readonly client: ControlPlaneClient;
  readonly launchNativeApprovalSheet: (
    ticket: ApprovalTicket,
  ) => Promise<NativeApprovalSheetOutcome>;
  readonly launchNativeSteeringConfirmSheet: (
    session: SessionSnapshot,
    preview: SteeringPreviewResp,
    text: string,
  ) => Promise<{
    sheet: NativeSteeringSheetOutcome;
    receipt?: SteeringConfirmResp;
  }>;
  readonly previewSteering: (req: SteeringPreviewReq) => Promise<SteeringPreviewResp>;
  readonly confirmSteering: (req: SteeringConfirmReq) => Promise<SteeringConfirmResp>;
  readonly sendPtyInput: (
    sessionKey: string,
    dataB64: string,
    biometricAttestedAtMs: number | null,
  ) => void;
  readonly revokeDevice: (deviceId: string) => Promise<void>;
  readonly dismissBanner: (bannerId: string) => void;
  readonly resyncNow: () => void;
}

const ControlPlaneContext = createContext<ControlPlaneContextValue | null>(null);

function createBrowserOrRnWebSocket(url: string): WebSocketLike {
  const GlobalWs = (globalThis as unknown as {
    WebSocket?: new (u: string) => WebSocketLike;
  }).WebSocket;
  if (!GlobalWs) {
    throw new Error("Global WebSocket unavailable in current runtime.");
  }
  return new GlobalWs(url);
}

export function ControlPlaneProvider({
  relayWsBaseUrl = "wss://relay.gitpulse.dev/v1/tunnel/ws",
  children,
}: {
  relayWsBaseUrl?: string;
  children: ReactNode;
}) {
  const [deviceKeys, setDeviceKeys] = useState<DevicePublicKeys | null>(null);

  const client = useMemo(
    () =>
      new ControlPlaneClient({
        relayWsBaseUrl,
        deviceId: deviceKeys?.device_id ?? "mobile-pending",
        transport: {
          connect: createBrowserOrRnWebSocket,
        },
      }),
    [relayWsBaseUrl, deviceKeys?.device_id],
  );

  const [state, setState] = useState<ControlPlaneState>(() => client.getState());

  useEffect(() => {
    const unsub = client.subscribe(setState);
    return unsub;
  }, [client]);

  // Initialize hardware P-256 + Keychain/Keystore identity public keys (NEVER private bytes)
  useEffect(() => {
    let active = true;
    GitPulseCrypto.ensureDeviceKeys()
      .then((keys) => {
        if (active) setDeviceKeys(keys);
      })
      .catch(() => {
        // Native module unavailable in non-dev-client preview environment
      });
    return () => {
      active = false;
    };
  }, []);

  // Register lock-screen notification categories (Opus M1: Deny & Deny+Stop ONLY — NEVER Approve)
  useEffect(() => {
    assertNoLockScreenApproveActions(NOTIFICATION_CATEGORIES);
    let subRemove: (() => void) | undefined;

    try {
      // eslint-disable-next-line @typescript-eslint/no-require-imports
      const Notifications = require("expo-notifications") as {
        setNotificationCategoryAsync?: (
          id: string,
          actions: ReadonlyArray<{
            identifier: string;
            buttonTitle: string;
            options: {
              opensAppToForeground: boolean;
              isDestructive: boolean;
              isAuthenticationRequired: boolean;
            };
          }>,
        ) => Promise<void>;
        addNotificationResponseReceivedListener?: (
          cb: (response: {
            actionIdentifier: string;
            notification: { request: { content: { data?: Record<string, unknown> } } };
          }) => void,
        ) => { remove: () => void };
      };

      for (const category of NOTIFICATION_CATEGORIES) {
        void Notifications.setNotificationCategoryAsync?.(
          category.identifier,
          category.actions.map((a) => ({
            identifier: a.identifier,
            buttonTitle: a.buttonTitle,
            options: { ...a.options },
          })),
        );
      }

      const subscription = Notifications.addNotificationResponseReceivedListener?.((response) => {
        const actionId = response.actionIdentifier;
        if (actionId !== ACTION_DENY_ID && actionId !== ACTION_DENY_AND_INTERRUPT_ID) {
          return;
        }
        const data = response.notification.request.content.data ?? {};
        const ticketId = typeof data.ticket_id === "string" ? data.ticket_id : "";
        const hostId = typeof data.host_id === "string" ? data.host_id : "";
        const inputSha256 = typeof data.input_sha256 === "string" ? data.input_sha256 : "";
        const expiresAt =
          typeof data.expires_at === "number"
            ? data.expires_at
            : Math.floor(Date.now() / 1000) + 300;
        if (!ticketId || !hostId || !inputSha256) return;

        const decision = actionId === ACTION_DENY_AND_INTERRUPT_ID ? "deny_and_interrupt" : "deny";
        void GitPulseCrypto.signDenyWithoutApprove({
          host_id: hostId,
          ticket_id: ticketId,
          input_sha256: inputSha256,
          decision,
          expires_at: expiresAt,
        }).then((signed) => {
          client.submitApprovalDecision(signed);
        });
      });
      subRemove = () => subscription?.remove();
    } catch {
      // expo-notifications unavailable outside native mobile runtime
    }

    return () => {
      subRemove?.();
    };
  }, [client]);

  const value = useMemo<ControlPlaneContextValue>(
    () => ({
      state,
      deviceKeys,
      client,
      async launchNativeApprovalSheet(ticket: ApprovalTicket): Promise<NativeApprovalSheetOutcome> {
        const outcome = await GitPulseCrypto.presentApprovalSheet({
          host_id: ticket.host_id,
          ticket_id: ticket.ticket_id,
          tool_name: ticket.tool_name,
          tool_input_json:
            typeof ticket.tool_input === "string"
              ? ticket.tool_input
              : JSON.stringify(ticket.tool_input),
          cwd: ticket.cwd,
          expected_input_sha256: ticket.input_sha256,
          repo_label: ticket.repo_label ?? ticket.cwd,
          harness_label: ticket.harness,
          expires_at: ticket.expires_at,
        });
        if (outcome.status === "signed" && outcome.signed_decision) {
          client.submitApprovalDecision(outcome.signed_decision);
        }
        return outcome;
      },
      async launchNativeSteeringConfirmSheet(
        session: SessionSnapshot,
        preview: SteeringPreviewResp,
        text: string,
      ) {
        const sheet = await GitPulseCrypto.presentSteeringConfirmSheet({
          host_id: session.host_id ?? "local-host",
          session_key: preview.session_key,
          request_id: preview.request_id,
          expect_route: preview.expect_route,
          delivery_mode: preview.delivery_mode,
          text,
          repo_label: session.repo_label,
          harness_label: session.harness,
          warning: preview.warning,
        });
        if (sheet.status === "signed" && sheet.signed_confirm) {
          const receipt = await client.confirmSteering({
            request_id: preview.request_id,
            session_key: preview.session_key,
            expect_route: preview.expect_route,
            text,
            confirm_token: preview.confirm_token,
            signed: sheet.signed_confirm,
          });
          return { sheet, receipt };
        }
        return { sheet };
      },
      previewSteering: (req) => client.previewSteering(req),
      confirmSteering: (req) => client.confirmSteering(req),
      sendPtyInput: (sessionKey, dataB64, biometricAttestedAtMs) =>
        client.sendPtyInput(sessionKey, dataB64, biometricAttestedAtMs),
      revokeDevice: (deviceId) => client.revokeDevice(deviceId),
      dismissBanner: (bannerId) => client.dismissBanner(bannerId),
      resyncNow: () => client.requestInboxSnapshot(),
    }),
    [state, deviceKeys, client],
  );

  return <ControlPlaneContext.Provider value={value}>{children}</ControlPlaneContext.Provider>;
}

export function useControlPlane(): ControlPlaneContextValue {
  const ctx = useContext(ControlPlaneContext);
  if (!ctx) {
    throw new Error("useControlPlane must be used inside <ControlPlaneProvider>.");
  }
  return ctx;
}
