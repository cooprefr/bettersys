import React from 'react';
import ReactDOM from 'react-dom/client';
import { PrivyProvider } from '@privy-io/react-auth';
import App from './App';
import './styles/globals.css';

import { PRIVY_APP_ID, PRIVY_ENABLED } from './config/privy';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {PRIVY_ENABLED ? (
      <PrivyProvider appId={PRIVY_APP_ID}>
        <App />
      </PrivyProvider>
    ) : (
      <App />
    )}
  </React.StrictMode>
);
