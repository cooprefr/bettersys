export const PRIVY_APP_ID = String(import.meta.env.VITE_PRIVY_APP_ID || '').trim();

export const PRIVY_ENABLED = PRIVY_APP_ID.length > 0;
