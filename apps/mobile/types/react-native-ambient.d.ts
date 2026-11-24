// Ambient type declarations for Expo / React Native packages so `tsc --noEmit`
// runs cleanly in strict mode even when native RN dependencies are not installed
// in a headless workspace. Real package types override these once installed.

declare module "react" {
  export type ReactNode =
    | string
    | number
    | boolean
    | null
    | undefined
    | ReactElement
    | ReadonlyArray<ReactNode>;
  export interface ReactElement {
    type: unknown;
    props: unknown;
    key: string | number | null;
  }
  export interface Attributes {
    key?: string | number | null;
  }
  export type ComponentType<P = Record<string, unknown>> = (
    props: P & Attributes,
  ) => ReactElement | null;
  export interface MutableRefObject<T> {
    current: T;
  }
  export interface Context<T> {
    Provider: ComponentType<{ value: T; children?: ReactNode }>;
  }
  export function createContext<T>(defaultValue: T): Context<T>;
  export function useContext<T>(context: Context<T>): T;
  export function useState<T>(initial: T | (() => T)): [T, (next: T | ((prev: T) => T)) => void];
  export function useEffect(effect: () => void | (() => void), deps?: readonly unknown[]): void;
  export function useMemo<T>(factory: () => T, deps?: readonly unknown[]): T;
  export function useRef<T>(initial: T): MutableRefObject<T>;

  namespace React {
    export type ReactNode =
      | string
      | number
      | boolean
      | null
      | undefined
      | ReactElement
      | ReadonlyArray<ReactNode>;
    export type ComponentType<P = Record<string, unknown>> = (
      props: P & Attributes,
    ) => ReactElement | null;
    export type ReactElement = {
      type: unknown;
      props: unknown;
      key: string | number | null;
    };
  }
  export default React;
}

declare module "react/jsx-runtime" {
  import type { ReactElement } from "react";
  export function jsx(type: unknown, props: unknown, key?: string | number): ReactElement;
  export function jsxs(type: unknown, props: unknown, key?: string | number): ReactElement;
  export const Fragment: (props: { children?: unknown }) => ReactElement | null;
  export namespace JSX {
    type Element = ReactElement;
    interface IntrinsicAttributes {
      key?: string | number | null;
    }
    interface IntrinsicElements {
      [elemName: string]: Record<string, unknown>;
    }
    interface ElementChildrenAttribute {
      children: unknown;
    }
  }
}

declare module "react-native" {
  import type { ComponentType, ReactNode } from "react";
  export type StyleProp =
    | Record<string, unknown>
    | ReadonlyArray<StyleProp | false | null | undefined>;
  export const View: ComponentType<{
    style?: StyleProp;
    accessibilityLabel?: string;
    children?: ReactNode;
  }>;
  export const Text: ComponentType<{
    style?: StyleProp;
    numberOfLines?: number;
    children?: ReactNode;
  }>;
  export const ScrollView: ComponentType<{
    style?: StyleProp;
    contentContainerStyle?: StyleProp;
    horizontal?: boolean;
    showsHorizontalScrollIndicator?: boolean;
    children?: ReactNode;
  }>;
  export const Pressable: ComponentType<{
    style?: StyleProp;
    onPress?: () => void | Promise<void>;
    disabled?: boolean;
    children?: ReactNode;
  }>;
  export const TextInput: ComponentType<{
    style?: StyleProp;
    value?: string;
    onChangeText?: (text: string) => void;
    placeholder?: string;
    placeholderTextColor?: string;
    multiline?: boolean;
  }>;
  export const StyleSheet: {
    create<T extends Record<string, Record<string, unknown>>>(styles: T): T;
  };
}

declare module "expo/config" {
  export interface ExpoConfig {
    name?: string;
    slug?: string;
    version?: string;
    orientation?: "portrait" | "landscape" | "default";
    scheme?: string;
    userInterfaceStyle?: "light" | "dark" | "automatic";
    newArchEnabled?: boolean;
    runtimeVersion?: { policy: string } | string;
    updates?: object;
    ios?: {
      supportsTablet?: boolean;
      bundleIdentifier?: string;
      entitlements?: Record<string, unknown>;
      infoPlist?: Record<string, unknown>;
    };
    android?: {
      package?: string;
      permissions?: string[];
      blockedPermissions?: string[];
    };
    plugins?: Array<string | [string, Record<string, unknown>]>;
    experiments?: Record<string, unknown>;
    extra?: Record<string, unknown>;
    [key: string]: unknown;
  }
  export interface ConfigContext {
    config: Partial<ExpoConfig>;
  }
}

declare module "expo-router" {
  import type { ComponentType, ReactNode } from "react";
  export const Stack: ComponentType<{
    screenOptions?: Record<string, unknown>;
    children?: ReactNode;
  }> & {
    Screen: ComponentType<{ name: string; options?: Record<string, unknown> }>;
  };
  export function useRouter(): {
    push(target: string | { pathname: string; params?: Record<string, string> }): void;
    back(): void;
  };
  export function useLocalSearchParams<T extends Record<string, string>>(): Partial<T>;
}

declare module "expo-status-bar" {
  import type { ComponentType } from "react";
  export const StatusBar: ComponentType<{ style?: "light" | "dark" | "auto" }>;
}

declare module "expo-camera" {
  import type { ComponentType } from "react";
  export const CameraView: ComponentType<{
    style?: unknown;
    barcodeScannerSettings?: { barcodeTypes: string[] };
    onBarcodeScanned?: (event: { data: string }) => void;
  }>;
  export function useCameraPermissions(): [
    { granted: boolean } | null,
    () => Promise<{ granted: boolean }>,
  ];
}

declare module "react-native-safe-area-context" {
  import type { ComponentType, ReactNode } from "react";
  export const SafeAreaProvider: ComponentType<{ children?: ReactNode }>;
}

declare module "react-native-webview" {
  import type { ComponentType, MutableRefObject } from "react";
  export const WebView: ComponentType<{
    ref?: MutableRefObject<{ injectJavaScript?: (js: string) => void } | null>;
    style?: unknown;
    originWhitelist?: string[];
    source: { html: string } | { uri: string };
    javaScriptEnabled?: boolean;
    allowFileAccess?: boolean;
    allowUniversalAccessFromFileURLs?: boolean;
  }>;
}

declare module "@shopify/flash-list" {
  import type { ReactElement, ReactNode } from "react";
  export function FlashList<T>(props: {
    data: readonly T[];
    keyExtractor: (item: T, index: number) => string;
    renderItem: (info: { item: T; index: number }) => ReactNode;
  }): ReactElement | null;
}
