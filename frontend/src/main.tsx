import React from 'react';
import ReactDOM from 'react-dom/client';
import { PrivyProvider } from '@privy-io/react-auth';
import App from './App';
import './styles/globals.css';

const privyAppId = import.meta.env.VITE_PRIVY_APP_ID || '';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <PrivyProvider appId={privyAppId}>
      <App />
    </PrivyProvider>
  </React.StrictMode>
);
