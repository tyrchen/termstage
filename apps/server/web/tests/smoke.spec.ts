import { expect, test } from '@playwright/test';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

test('terminal app renders and round-trips through the Rust PTY bridge', async ({
  page
}, testInfo) => {
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
    await page.keyboard.type('printf phase4-output');
    await page.keyboard.press('Enter');
    await expect(page.locator('.xterm-rows')).toContainText('phase4-output');
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
    await expect(page.locator('.xterm-rows')).toContainText('80');
  } finally {
    await server.stop();
  }
});

async function startTermstageServer(): Promise<{
  url: string;
  stop: () => Promise<void>;
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
      '--mode',
      'shell',
      '--shell',
      '/bin/bash',
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
    stop: async () => {
      child.kill('SIGINT');
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
