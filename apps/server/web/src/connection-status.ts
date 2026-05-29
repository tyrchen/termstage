import {
  FONT_FAMILIES,
  PRESENTATION_THEMES,
  PresentationThemeName,
  TerminalFontFamily,
  TerminalFontFamilyName
} from './presentation';

export type ConnectionStatus =
  | { state: 'connected' }
  | { state: 'reconnecting' }
  | { state: 'lost' }
  | { state: 'ended'; title: string; message: string };

export interface ConnectionStatusDialog {
  update: (status: ConnectionStatus) => void;
}

export interface TerminalToolbar {
  updateFontFamily: (fontFamily: TerminalFontFamily) => void;
  updateFontSize: (fontSize: number) => void;
  updateLease: (owner: 'terminal' | 'browser' | 'agent') => void;
  updateSession: (session: string) => void;
  updateTheme: (theme: PresentationThemeName) => void;
}

export interface TerminalToolbarOptions {
  onChangeFontFamily: (fontFamilyName: TerminalFontFamilyName) => void;
  onChangeTheme: (theme: PresentationThemeName) => void;
  onDecreaseFont: () => void;
  onIncreaseFont: () => void;
}

export function createConnectionStatusDialog(root: HTMLElement): ConnectionStatusDialog {
  const overlay = document.createElement('div');
  overlay.className = 'connection-status';
  overlay.hidden = true;
  overlay.setAttribute('role', 'dialog');
  overlay.setAttribute('aria-modal', 'true');
  overlay.setAttribute('aria-labelledby', 'connection-status-title');
  overlay.setAttribute('aria-describedby', 'connection-status-message');

  const panel = document.createElement('div');
  panel.className = 'connection-status__panel';

  const title = document.createElement('h1');
  title.id = 'connection-status-title';

  const message = document.createElement('p');
  message.id = 'connection-status-message';

  panel.append(title, message);
  overlay.append(panel);
  root.append(overlay);

  return {
    update: (status: ConnectionStatus) => {
      if (status.state === 'connected') {
        overlay.hidden = true;
        return;
      }

      const copy = copyForStatus(status);
      title.textContent = copy.title;
      message.textContent = copy.message;
      overlay.hidden = false;
    }
  };
}

export function createTerminalToolbar(
  root: HTMLElement,
  options: TerminalToolbarOptions
): TerminalToolbar {
  const toolbar = document.createElement('nav');
  toolbar.className = 'terminal-toolbar';
  toolbar.setAttribute('aria-label', 'Terminal session');

  const sessionItem = createToolbarField('Session', 'starting');
  const controlItem = createToolbarField('Control', 'control by terminal');
  controlItem.value.setAttribute('role', 'status');
  controlItem.value.setAttribute('aria-live', 'polite');

  const spacer = document.createElement('div');
  spacer.className = 'terminal-toolbar__spacer';

  const themeSelect = document.createElement('select');
  themeSelect.className = 'terminal-toolbar__select';
  themeSelect.setAttribute('aria-label', 'Theme');
  for (const theme of PRESENTATION_THEMES) {
    const option = document.createElement('option');
    option.value = theme.name;
    option.textContent = theme.label;
    themeSelect.append(option);
  }
  themeSelect.addEventListener('change', () => {
    options.onChangeTheme(themeSelect.value as PresentationThemeName);
  });

  const fontFamilySelect = document.createElement('select');
  fontFamilySelect.className = 'terminal-toolbar__select terminal-toolbar__select--wide';
  fontFamilySelect.setAttribute('aria-label', 'Font family');
  for (const fontFamily of FONT_FAMILIES) {
    const option = document.createElement('option');
    option.value = fontFamily.name;
    option.textContent = fontFamily.label;
    fontFamilySelect.append(option);
  }
  fontFamilySelect.addEventListener('change', () => {
    options.onChangeFontFamily(fontFamilySelect.value as TerminalFontFamilyName);
  });

  const fontGroup = document.createElement('div');
  fontGroup.className = 'terminal-toolbar__font';
  fontGroup.setAttribute('aria-label', 'Font size');

  const decreaseButton = createToolbarButton('Decrease font size', '-');
  const fontSizeValue = document.createElement('span');
  fontSizeValue.className = 'terminal-toolbar__font-size';
  fontSizeValue.setAttribute('aria-live', 'polite');
  const increaseButton = createToolbarButton('Increase font size', '+');

  decreaseButton.addEventListener('click', () => {
    options.onDecreaseFont();
  });
  increaseButton.addEventListener('click', () => {
    options.onIncreaseFont();
  });

  fontGroup.append(decreaseButton, fontSizeValue, increaseButton);
  toolbar.append(
    sessionItem.element,
    controlItem.element,
    spacer,
    themeSelect,
    fontFamilySelect,
    fontGroup
  );
  root.append(toolbar);

  return {
    updateFontFamily: (fontFamily: TerminalFontFamily) => {
      fontFamilySelect.value = fontFamily.name;
    },
    updateFontSize: (fontSize: number) => {
      fontSizeValue.textContent = `${fontSize}px`;
    },
    updateLease: (owner: 'terminal' | 'browser' | 'agent') => {
      controlItem.value.dataset.owner = owner;
      controlItem.value.textContent = controlLabel(owner);
    },
    updateSession: (session: string) => {
      sessionItem.value.textContent = session;
    },
    updateTheme: (theme: PresentationThemeName) => {
      themeSelect.value = theme;
    }
  };
}

function controlLabel(owner: 'terminal' | 'browser' | 'agent'): string {
  switch (owner) {
    case 'browser':
      return 'control by browser';
    case 'agent':
      return 'control by agent';
    case 'terminal':
      return 'control by terminal';
  }
}

function createToolbarField(
  labelText: string,
  valueText: string
): { element: HTMLDivElement; value: HTMLSpanElement } {
  const element = document.createElement('div');
  element.className = 'terminal-toolbar__field';

  const label = document.createElement('span');
  label.className = 'terminal-toolbar__label';
  label.textContent = labelText;

  const value = document.createElement('span');
  value.className = 'terminal-toolbar__value';
  value.textContent = valueText;

  element.append(label, value);
  return { element, value };
}

function createToolbarButton(label: string, text: string): HTMLButtonElement {
  const button = document.createElement('button');
  button.className = 'terminal-toolbar__button';
  button.type = 'button';
  button.setAttribute('aria-label', label);
  button.textContent = text;
  return button;
}

function copyForStatus(status: Exclude<ConnectionStatus, { state: 'connected' }>): {
  title: string;
  message: string;
} {
  switch (status.state) {
    case 'reconnecting':
      return {
        title: 'Reconnecting',
        message: 'The terminal connection dropped. Trying to reconnect.'
      };
    case 'lost':
      return {
        title: 'Lost connectivity',
        message: 'The server is not reachable. Refresh after restarting it.'
      };
    case 'ended':
      return {
        title: status.title,
        message: status.message
      };
  }
}
