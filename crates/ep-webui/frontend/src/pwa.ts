// PWA Service Worker 注册（T-29：WebUI 可安装到桌面）。
//
// fail-safe 约定：SW 只是增强（安装元数据 + 静态资产缓存），任何失败——
// 浏览器不支持、被策略禁用、注册网络错误——都必须静默吞掉，绝不影响应用
// 本身；业务功能不得依赖 SW 的存在。
//
// 开发模式（vite dev）跳过注册：dev 产物无内容哈希且频繁热更新，被 SW
// 的 stale-while-revalidate 缓存会造成「改了代码不生效」的困惑。
export function registerServiceWorker(): void {
  if (import.meta.env.DEV) return
  if (!('serviceWorker' in navigator)) return
  try {
    // window load 后再注册，避免与首屏关键资源争抢带宽。
    window.addEventListener('load', () => {
      navigator.serviceWorker.register('/sw.js').catch(() => {
        // SW 注册失败不影响应用（见文件头 fail-safe 约定）。
      })
    })
  } catch {
    // 同上，fail-safe。
  }
}

registerServiceWorker()
