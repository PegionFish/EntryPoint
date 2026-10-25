/*
 * EntryPoint WebUI Service Worker（T-29：PWA 可安装性）。
 *
 * 红线（ep-desktop 退役教训——壳不承载业务功能）：
 * 本 SW 只承担「安装元数据载体 + 静态资产缓存」两件事，绝不触碰业务数据。
 *
 * ── 缓存策略（保守）───────────────────────────────────────────────────
 * - /api、/ws 及其子路径：显式放行（不 respondWith、不缓存）。daemon 场景下
 *   缓存 API/WS 会造成任务状态错乱（任务列表/日志读到旧快照），这是硬禁区。
 * - 任何 URL 带 ?token= 查询参数：放行。访问令牌（T-06 token 模式）不得
 *   以任何形式落入 Cache Storage。
 * - 导航请求（HTML）：network-first——每次导航都拿最新 index.html，仅离线时
 *   回退缓存（精确命中 → 根页面兜底，SPA 路由在客户端接管深链）。
 * - /assets/*（vite 内容哈希产物，文件名不可变）与图标/字体类静态文件：
 *   stale-while-revalidate——先回缓存包，后台拉新刷新下次生效。
 * - 其余同源 GET（如 /health、/icons.svg 雪碧图等杂项）：放行，不缓存。
 *
 * ── 更新策略─────────────────────────────────────────────────────────
 * 发布新前端时 bump CACHE_VERSION（如 v1 → v2）：install 预缓存新清单，
 * activate 删除全部旧版本缓存；skipWaiting + clients.claim() 让新 SW 立即
 * 接管。HTML 本身 network-first，用户下次刷新即进入新版本，无「双版本并存」
 * 风险；旧哈希资产因离线兜底可能仍留缓存中（体积有界，无害），bump 版本号
 * 后随旧缓存一并清除。
 */

const CACHE_VERSION = 'ep-static-v1';

// 预缓存：安装壳所需的最小静态集。HTML 仅作离线兜底（线上仍 network-first）。
const PRECACHE_URLS = [
  '/',
  '/manifest.webmanifest',
  '/favicon.svg',
  '/icons/icon-192.png',
  '/icons/icon-512.png',
];

// 绝不触碰缓存的请求：业务 API/WebSocket，以及任何带令牌的 URL。
function isBypassed(url) {
  return (
    url.pathname === '/api' ||
    url.pathname.startsWith('/api/') ||
    url.pathname === '/ws' ||
    url.pathname.startsWith('/ws/') ||
    url.searchParams.has('token')
  );
}

// 可安全缓存的静态资产：构建产物（内容哈希）+ 图标 + 字体。
function isStaticAsset(url) {
  return (
    url.pathname.startsWith('/assets/') ||
    url.pathname === '/favicon.svg' ||
    url.pathname.startsWith('/icons/') ||
    url.pathname.endsWith('.woff2') ||
    url.pathname.endsWith('.woff') ||
    url.pathname.endsWith('.ttf')
  );
}

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches
      .open(CACHE_VERSION)
      .then((cache) => cache.addAll(PRECACHE_URLS))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(
          keys.filter((key) => key !== CACHE_VERSION).map((key) => caches.delete(key)),
        ),
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener('fetch', (event) => {
  const { request } = event;

  // 只管同源 GET；其余（POST/WS 升级/跨源）一律不拦截。
  if (request.method !== 'GET') return;

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;
  if (isBypassed(url)) return;

  // 导航请求：network-first，离线时回退缓存（深链兜底到根页面，SPA 接管）。
  if (request.mode === 'navigate') {
    event.respondWith(
      fetch(request).catch(() =>
        caches
          .match(request)
          .then((hit) => hit || caches.match('/'))
          .then((hit) => hit || Response.error()),
      ),
    );
    return;
  }

  // 静态资产：stale-while-revalidate（先回缓存，后台拉新）。
  if (isStaticAsset(url)) {
    event.respondWith(
      caches.match(request).then((cached) => {
        const network = fetch(request)
          .then((response) => {
            if (response && response.ok) {
              const copy = response.clone();
              caches.open(CACHE_VERSION).then((cache) => cache.put(request, copy));
            }
            return response;
          })
          .catch(() => cached);
        return cached || network;
      }),
    );
  }
  // 其余同源 GET：放行（不调用 respondWith，浏览器默认行为）。
});
