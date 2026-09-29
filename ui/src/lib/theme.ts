// The color theme: follow the system, or a fixed light or dark choice
// remembered in this browser. Applied as a data attribute (not an inline
// style) so the Content-Security-Policy stays strict.

export type ThemeChoice = 'system' | 'light' | 'dark';

const KEY = 'oxim.theme';

export function storedTheme(): ThemeChoice {
  try {
    const value = localStorage.getItem(KEY);
    return value === 'light' || value === 'dark' ? value : 'system';
  } catch {
    return 'system';
  }
}

export function applyTheme(choice: ThemeChoice): void {
  const root = document.documentElement;
  if (choice === 'system') delete root.dataset.theme;
  else root.dataset.theme = choice;
  try {
    if (choice === 'system') localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, choice);
  } catch {
    // Storage may be unavailable (private mode); the choice lasts for the page.
  }
}
