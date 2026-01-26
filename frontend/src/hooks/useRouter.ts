import { useState, useEffect, useCallback } from 'react';

/**
 * Simple URL-based router using History API.
 * Supports /runs/{run_id} route for permalinked backtest results.
 */

export interface RouteMatch {
  route: 'app' | 'run' | 'not_found';
  params: Record<string, string>;
}

// Parse the current URL and extract route information
function parseRoute(pathname: string): RouteMatch {
  // Match /runs/{run_id}
  const runMatch = pathname.match(/^\/runs\/([^/]+)\/?$/);
  if (runMatch) {
    const runId = decodeURIComponent(runMatch[1]);
    // Validate run_id is not empty or just whitespace
    if (runId.trim().length > 0) {
      return { route: 'run', params: { runId } };
    }
    return { route: 'not_found', params: {} };
  }

  // Match root or app routes
  if (pathname === '/' || pathname === '' || pathname.startsWith('/?')) {
    return { route: 'app', params: {} };
  }

  // Unknown routes
  return { route: 'not_found', params: {} };
}

export function useRouter() {
  const [routeMatch, setRouteMatch] = useState<RouteMatch>(() =>
    parseRoute(window.location.pathname)
  );

  // Handle browser back/forward navigation
  useEffect(() => {
    const handlePopState = () => {
      setRouteMatch(parseRoute(window.location.pathname));
    };

    window.addEventListener('popstate', handlePopState);
    return () => window.removeEventListener('popstate', handlePopState);
  }, []);

  // Navigate to a new URL
  const navigate = useCallback((path: string, replace = false) => {
    if (replace) {
      window.history.replaceState(null, '', path);
    } else {
      window.history.pushState(null, '', path);
    }
    setRouteMatch(parseRoute(path));
  }, []);

  // Navigate to run page
  const navigateToRun = useCallback((runId: string) => {
    navigate(`/runs/${encodeURIComponent(runId)}`);
  }, [navigate]);

  // Navigate to app (home)
  const navigateToApp = useCallback(() => {
    navigate('/');
  }, [navigate]);

  // Get current URL for sharing
  const getCurrentUrl = useCallback(() => {
    return window.location.href;
  }, []);

  return {
    route: routeMatch.route,
    params: routeMatch.params,
    navigate,
    navigateToRun,
    navigateToApp,
    getCurrentUrl,
  };
}

// Helper to copy text to clipboard
export async function copyToClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // Fallback for older browsers
    const textarea = document.createElement('textarea');
    textarea.value = text;
    textarea.style.position = 'fixed';
    textarea.style.opacity = '0';
    document.body.appendChild(textarea);
    textarea.select();
    try {
      document.execCommand('copy');
      return true;
    } catch {
      return false;
    } finally {
      document.body.removeChild(textarea);
    }
  }
}
