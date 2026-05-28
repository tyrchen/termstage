import { expect, test, type Page } from '@playwright/test';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

declare global {
  interface Window {
    __termstageMockSocketCount?: number;
    __termstageObservedResizeMessages?: string[];
    __termstageObservedSocketCloseCount?: number;
  }
}

test('terminal app renders and round-trips through the Rust PTY bridge', async ({
  page
}, testInfo) => {
  const server = await startTermstageServer();
  const failedAssets: string[] = [];
  page.on('response', response => {
    if (response.status() >= 400 && response.url().includes('/assets/')) {
      failedAssets.push(`${response.status()} ${response.url()}`);
    }
  });
  try {
    testInfo.attach('launch-url-redacted', {
      body: server.url.replace(/token=[^&]+/, 'token=[REDACTED]'),
      contentType: 'text/plain'
    });
    await page.goto(server.url);
    expect(failedAssets).toEqual([]);
    const root = page.locator('#terminal-root');
    await expect(root).toBeVisible();
    const toolbar = page.getByRole('navigation', { name: 'Terminal session' });
    await expect(toolbar).toBeVisible();
    await expect(toolbar).toContainText('Session');
    await expect(toolbar).toContainText('control by terminal');
    await expect(toolbar).toContainText('24px');
    await page.getByRole('button', { name: 'Increase font size' }).click();
    await expect(toolbar).toContainText('25px');
    await page.getByRole('button', { name: 'Decrease font size' }).click();
    await expect(toolbar).toContainText('24px');
    await expect(page.locator('.xterm')).toBeVisible();
    await page.keyboard.type('printf phase4-output');
    await page.keyboard.press('Enter');
    await expect(page.locator('.xterm-rows')).toContainText('phase4-output');
    await page.keyboard.type('printf "$TERM|$COLORTERM|$CLICOLOR"');
    await page.keyboard.press('Enter');
    await expect(page.locator('.xterm-rows')).toContainText('xterm-256color|truecolor|1');
    const desktop = await page.screenshot({
      path: testInfo.outputPath('desktop-terminal.png')
    });
    expect(desktop.byteLength).toBeGreaterThan(1000);
    await page.setViewportSize({ width: 390, height: 720 });
    await expect(root).toBeVisible();
    const narrow = await page.screenshot({
      path: testInfo.outputPath('narrow-terminal.png')
    });
    expect(narrow.byteLength).toBeGreaterThan(1000);
  } finally {
    await server.stop();
  }
});

test('terminal app renders common Unicode terminal glyphs', async ({ page }, testInfo) => {
  const server = await startTermstageServer();
  try {
    testInfo.attach('launch-url-redacted', {
      body: server.url.replace(/token=[^&]+/, 'token=[REDACTED]'),
      contentType: 'text/plain'
    });
    await page.goto(server.url);
    await expect(page.locator('.xterm')).toBeVisible();
    await page.keyboard.insertText("printf '─│╭╮█⠀'");
    await page.keyboard.press('Enter');
    await expect(page.locator('.xterm-rows')).toContainText('─│╭╮█⠀');
    const screenshot = await page.screenshot({
      path: testInfo.outputPath('unicode-terminal.png')
    });
    expect(screenshot.byteLength).toBeGreaterThan(1000);
  } finally {
    await server.stop();
  }
});

test('terminal app keeps viewport stable when font size crosses a column boundary', async ({
  page
}, testInfo) => {
  const server = await startTermstageServer();
  try {
    testInfo.attach('launch-url-redacted', {
      body: server.url.replace(/token=[^&]+/, 'token=[REDACTED]'),
      contentType: 'text/plain'
    });
    await page.setViewportSize({ width: 1525, height: 900 });
    await page.goto(server.url.replace(/fontSize=\d+/, 'fontSize=13'));
    await expect(page.locator('.xterm')).toBeVisible();
    await expectTerminalViewportNotOverflowing(page);
    await page.getByRole('button', { name: 'Increase font size' }).click();
    await expect(page.getByRole('navigation', { name: 'Terminal session' })).toContainText(
      '14px'
    );
    await expectTerminalViewportNotOverflowing(page);
  } finally {
    await server.stop();
  }
});

test('terminal app keeps websocket connected when the viewport is shorter than protocol minimum rows', async ({
  page
}, testInfo) => {
  const server = await startTermstageServer();
  try {
    testInfo.attach('launch-url-redacted', {
      body: server.url.replace(/token=[^&]+/, 'token=[REDACTED]'),
      contentType: 'text/plain'
    });
    await page.addInitScript(() => {
      const NativeWebSocket = window.WebSocket;
      window.__termstageObservedResizeMessages = [];
      window.__termstageObservedSocketCloseCount = 0;

      class ObservedTerminalWebSocket extends NativeWebSocket {
        constructor(url: string | URL, protocols?: string | string[]) {
          if (protocols === undefined) {
            super(url);
          } else {
            super(url, protocols);
          }
          this.addEventListener('close', () => {
            window.__termstageObservedSocketCloseCount =
              (window.__termstageObservedSocketCloseCount ?? 0) + 1;
          });
        }

        override send(data: string | ArrayBufferLike | Blob | ArrayBufferView): void {
          if (typeof data === 'string' && data.includes('"type":"resize"')) {
            window.__termstageObservedResizeMessages?.push(data);
          }
          super.send(data);
        }
      }

      window.WebSocket = ObservedTerminalWebSocket as unknown as typeof WebSocket;
    });
    await page.setViewportSize({ width: 1024, height: 128 });
    const url = new URL(server.url);
    url.searchParams.set('fontFamily', 'monospace');
    url.searchParams.set('fontSize', '18');
    await page.goto(url.toString());
    await expect(page.locator('.xterm')).toBeVisible();
    await page.waitForTimeout(800);
    await expect(page.getByRole('dialog')).toBeHidden();
    expect(await page.evaluate(() => window.__termstageObservedSocketCloseCount ?? 0)).toBe(0);
    const resizeMessages = await page.evaluate(
      () => window.__termstageObservedResizeMessages ?? []
    );
    expect(resizeMessages.length).toBeGreaterThan(0);
    for (const message of resizeMessages) {
      const resize = JSON.parse(message) as { cols?: unknown; rows?: unknown };
      expect(resize.cols).toBeGreaterThanOrEqual(20);
      expect(resize.rows).toBeGreaterThanOrEqual(5);
    }
  } finally {
    await server.stop();
  }
});

test('terminal app scrolls through browser wheel input', async ({ page }, testInfo) => {
  const server = await startTermstageServer();
  try {
    testInfo.attach('launch-url-redacted', {
      body: server.url.replace(/token=[^&]+/, 'token=[REDACTED]'),
      contentType: 'text/plain'
    });
    await page.goto(server.url);
    const root = page.locator('#terminal-root');
    await expect(root).toBeVisible();
    await expect(page.locator('.xterm')).toBeVisible();
    await page.keyboard.type('seq 1 120');
    await page.keyboard.press('Enter');
    await expect(page.locator('.xterm-rows')).toContainText('120');
    await expect(page.locator('.xterm-rows')).not.toContainText('\n80');
    const box = await root.boundingBox();
    expect(box).not.toBeNull();
    if (box === null) {
      throw new Error('terminal root bounding box was not available');
    }
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.wheel(0, -800);
    await expect(page.locator('.xterm-rows')).toContainText('81');
  } finally {
    await server.stop();
  }
});

test('terminal app stops reconnecting when another browser takes over', async ({
  page,
  context
}, testInfo) => {
  const server = await startTermstageServer();
  const secondPage = await context.newPage();
  try {
    testInfo.attach('launch-url-redacted', {
      body: server.url.replace(/token=[^&]+/, 'token=[REDACTED]'),
      contentType: 'text/plain'
    });
    await page.goto(server.url);
    await expect(page.locator('.xterm')).toBeVisible();
    await page.locator('.xterm').click();
    await page.keyboard.type('printf controller-before-replace');
    await page.keyboard.press('Enter');
    await expect(page.locator('.xterm-rows')).toContainText('controller-before-replace');

    await secondPage.goto(server.url);
    await expect(secondPage.locator('.xterm')).toBeVisible();
    await expect(page.getByRole('dialog')).toContainText('Session attached elsewhere');
    await secondPage.locator('.xterm').click();
    await secondPage.keyboard.type('printf controller-after-replace');
    await secondPage.keyboard.press('Enter');
    await expect(secondPage.locator('.xterm-rows')).toContainText('controller-after-replace');
  } finally {
    await secondPage.close();
    await server.stop();
  }
});

test('terminal app reconnects after ambiguous socket close', async ({ page }, testInfo) => {
  const server = await startTermstageServer();
  try {
    testInfo.attach('launch-url-redacted', {
      body: server.url.replace(/token=[^&]+/, 'token=[REDACTED]'),
      contentType: 'text/plain'
    });
    await page.addInitScript(() => {
      let socketCount = 0;

      class MockTerminalWebSocket extends EventTarget {
        static readonly CONNECTING = 0;
        static readonly OPEN = 1;
        static readonly CLOSING = 2;
        static readonly CLOSED = 3;

        readonly url: string;
        binaryType: BinaryType = 'arraybuffer';
        readyState = MockTerminalWebSocket.CONNECTING;

        constructor(url: string | URL) {
          super();
          this.url = url.toString();
          socketCount += 1;
          window.__termstageMockSocketCount = socketCount;
          window.setTimeout(() => {
            this.readyState = MockTerminalWebSocket.OPEN;
            this.dispatchEvent(new Event('open'));
            if (socketCount === 1) {
              window.setTimeout(() => {
                this.readyState = MockTerminalWebSocket.CLOSED;
                this.dispatchEvent(
                  new CloseEvent('close', {
                    code: 1000,
                    reason: '',
                    wasClean: true
                  })
                );
              }, 20);
            }
          }, 0);
        }

        close(): void {
          this.readyState = MockTerminalWebSocket.CLOSED;
          this.dispatchEvent(new CloseEvent('close', { code: 1000, wasClean: true }));
        }

        send(_data: string | ArrayBufferLike | Blob | ArrayBufferView): void {}
      }

      window.WebSocket = MockTerminalWebSocket as unknown as typeof WebSocket;
    });
    await page.goto(server.url);
    await expect(page.locator('.xterm')).toBeVisible();
    await expect
      .poll(() => page.evaluate(() => window.__termstageMockSocketCount ?? 0))
      .toBeGreaterThan(1);
    await expect(page.getByRole('dialog')).toBeHidden();
  } finally {
    await server.stop();
  }
});

test('terminal app holds the session when shell exits', async ({ page }, testInfo) => {
  const server = await startTermstageServer();
  try {
    testInfo.attach('launch-url-redacted', {
      body: server.url.replace(/token=[^&]+/, 'token=[REDACTED]'),
      contentType: 'text/plain'
    });
    await page.goto(server.url);
    await expect(page.locator('.xterm')).toBeVisible();
    await expect(page.locator('.xterm-rows')).toContainText('$');
    await page.locator('.xterm').click();
    await page.keyboard.type('exit');
    await page.keyboard.press('Enter');
    await expect(page.getByRole('dialog')).toContainText('Process exited');
    await expect(page.getByRole('dialog')).toContainText('The terminal process exited.');
    await page.reload();
    await expect(page.locator('.xterm')).toBeVisible();
    await expect(page.getByRole('dialog')).toBeHidden();
    await expect(page.locator('.xterm-rows')).toContainText('$');
    await page.locator('.xterm').click();
    await page.keyboard.type('printf after-refresh-restart');
    await page.keyboard.press('Enter');
    await expect(page.locator('.xterm-rows')).toContainText('after-refresh-restart');
  } finally {
    await server.stop();
  }
});

test('terminal app shows lost-connectivity status when server disappears', async ({
  page
}, testInfo) => {
  const server = await startTermstageServer();
  let stopped = false;
  try {
    testInfo.attach('launch-url-redacted', {
      body: server.url.replace(/token=[^&]+/, 'token=[REDACTED]'),
      contentType: 'text/plain'
    });
    await page.goto(server.url);
    await expect(page.locator('.xterm')).toBeVisible();
    await server.stop('SIGKILL');
    stopped = true;
    await expect(page.getByRole('dialog')).toContainText('Lost connectivity');
  } finally {
    if (!stopped) {
      await server.stop();
    }
  }
});

async function expectTerminalViewportNotOverflowing(page: Page): Promise<void> {
  const metrics = await page.evaluate(() => {
    const viewport = document.querySelector<HTMLElement>('.terminal-viewport');
    const xterm = document.querySelector<HTMLElement>('.xterm');
    return {
      viewportClientWidth: viewport?.clientWidth ?? 0,
      viewportScrollWidth: viewport?.scrollWidth ?? 0,
      viewportClientHeight: viewport?.clientHeight ?? 0,
      viewportScrollHeight: viewport?.scrollHeight ?? 0,
      xtermWidth: xterm?.getBoundingClientRect().width ?? 0,
      xtermHeight: xterm?.getBoundingClientRect().height ?? 0
    };
  });

  expect(metrics.viewportScrollWidth).toBeLessThanOrEqual(metrics.viewportClientWidth);
  expect(metrics.viewportScrollHeight).toBeLessThanOrEqual(metrics.viewportClientHeight);
  expect(metrics.xtermWidth).toBeLessThanOrEqual(metrics.viewportClientWidth);
  expect(metrics.xtermHeight).toBeLessThanOrEqual(metrics.viewportClientHeight);
}

async function startTermstageServer(): Promise<{
  url: string;
  stop: (signal?: NodeJS.Signals) => Promise<void>;
}> {
  const testDir = path.dirname(fileURLToPath(import.meta.url));
  const repoRoot = path.resolve(testDir, '../../../..');
  const child = spawn(
    'cargo',
    [
      'run',
      '-p',
      'termstage',
      '--bin',
      'termstage',
      '--',
      'web',
      'start',
      '--mode',
      'shell',
      '--command',
      '/bin/bash',
      '--exit-policy',
      'hold',
      '--port',
      '0',
      '--font-size',
      '24',
      '--theme',
      'high-contrast'
    ],
    {
      cwd: repoRoot,
      env: {
        ...process.env,
        RUST_LOG: 'termstage=warn'
      }
    }
  );
  const url = await readLaunchUrl(child);
  return {
    url,
    stop: async (signal: NodeJS.Signals = 'SIGINT') => {
      child.kill(signal);
      await waitForExit(child);
    }
  };
}

async function readLaunchUrl(child: ChildProcessWithoutNullStreams): Promise<string> {
  const chunks: string[] = [];
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      reject(new Error(`termstage server did not print launch URL: ${chunks.join('')}`));
    }, 30000);
    const onData = (chunk: Buffer): void => {
      chunks.push(chunk.toString('utf8'));
      const match = chunks.join('').match(/http:\/\/127\.0\.0\.1:\d+\/\?token=[^\s]+/);
      if (match !== null) {
        clearTimeout(timeout);
        resolve(match[0]);
      }
    };
    child.stdout.on('data', onData);
    child.stderr.on('data', onData);
    child.once('error', error => {
      clearTimeout(timeout);
      reject(error);
    });
    child.once('exit', code => {
      clearTimeout(timeout);
      reject(new Error(`termstage server exited before printing URL: ${code}`));
    });
  });
}

async function waitForExit(child: ChildProcessWithoutNullStreams): Promise<void> {
  if (child.exitCode !== null) {
    return;
  }
  await new Promise<void>(resolve => {
    child.once('exit', () => resolve());
    setTimeout(() => {
      child.kill('SIGKILL');
      resolve();
    }, 5000);
  });
}
