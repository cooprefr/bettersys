/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_API_URL: string;
  readonly VITE_WS_URL: string;
  readonly VITE_WS_PING_MS?: string;
  readonly VITE_ENABLE_TRADING?: string;
  readonly VITE_PRIVY_APP_ID?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
