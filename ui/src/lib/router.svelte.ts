// The client-side router: the current location as reactive state, and
// navigation through the History API. OXIM serves index.html for every
// non-API path, so deep links and reloads work.

import { isClientPath, matchRoute, type Route } from './routes';

class Router {
  pathname = $state(typeof location === 'undefined' ? '/' : location.pathname);
  search = $state(typeof location === 'undefined' ? '' : location.search);
  route: Route = $derived(matchRoute(this.pathname));
  /** Incremented on every navigation, including to the same path. */
  visits = $state(0);

  constructor() {
    if (typeof window === 'undefined') return;
    window.addEventListener('popstate', () => this.sync());
    document.addEventListener('click', (event) => this.intercept(event));
  }

  private sync(): void {
    this.pathname = location.pathname;
    this.search = location.search;
    this.visits += 1;
  }

  /** The current query parameters. */
  get query(): URLSearchParams {
    return new URLSearchParams(this.search);
  }

  navigate(to: string, options: { replace?: boolean } = {}): void {
    const url = new URL(to, location.origin);
    const target = url.pathname + url.search + url.hash;
    if (options.replace) history.replaceState(null, '', target);
    else history.pushState(null, '', target);
    this.sync();
  }

  /** Follows same-origin links without a page load. */
  private intercept(event: MouseEvent): void {
    if (event.defaultPrevented || event.button !== 0) return;
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const anchor = (event.target as Element | null)?.closest?.('a');
    if (!anchor || anchor.target || anchor.hasAttribute('download')) return;
    const href = anchor.getAttribute('href');
    if (!href || !isClientPath(href)) return;
    event.preventDefault();
    this.navigate(href);
  }
}

export const router = new Router();
