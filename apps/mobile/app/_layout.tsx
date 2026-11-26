import { Stack } from "expo-router";
import { StatusBar } from "expo-status-bar";
import { SafeAreaProvider } from "react-native-safe-area-context";
import { ControlPlaneProvider } from "@/state/ControlPlaneContext";
import { colors } from "@/theme/tokens";

export default function RootLayout() {
  return (
    <SafeAreaProvider>
      <ControlPlaneProvider>
        <StatusBar style="light" />
        <Stack
          screenOptions={{
            headerStyle: { backgroundColor: colors.surface },
            headerTintColor: colors.text,
            headerTitleStyle: { fontWeight: "700" },
            contentStyle: { backgroundColor: colors.bg },
          }}
        >
          <Stack.Screen name="index" options={{ title: "GitPulse Control Plane" }} />
          <Stack.Screen
            name="approvals/[ticketId]"
            options={{ title: "Approval Request", presentation: "modal" }}
          />
          <Stack.Screen name="sessions/[key]" options={{ title: "Session Detail" }} />
          <Stack.Screen name="pair" options={{ title: "QR Pairing & Devices" }} />
          <Stack.Screen
            name="pty/[sessionKey]"
            options={{ title: "PTY Drawer (Read-Only)", presentation: "modal" }}
          />
        </Stack>
      </ControlPlaneProvider>
    </SafeAreaProvider>
  );
}
