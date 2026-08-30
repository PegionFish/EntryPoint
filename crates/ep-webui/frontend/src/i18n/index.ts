import i18n from 'i18next'
import { initReactI18next } from 'react-i18next'
// 仓库级翻译文件（扁平键格式），由后端共享维护。
// apiCore/apiModels/apiPipelines 命名空间仅供后端使用，前端不加载
//（desktopPages/desktopApp 已随桌面端 2026-08-13 退役移除）。
import zhCommon from '@i18n/locales/zh-CN/common.json'
import zhDashboard from '@i18n/locales/zh-CN/dashboard.json'
import zhModules from '@i18n/locales/zh-CN/modules.json'
import zhModels from '@i18n/locales/zh-CN/models.json'
import zhPacks from '@i18n/locales/zh-CN/packs.json'
import zhPipeline from '@i18n/locales/zh-CN/pipeline.json'
import zhRun from '@i18n/locales/zh-CN/run.json'
import zhTasks from '@i18n/locales/zh-CN/tasks.json'
import zhSettings from '@i18n/locales/zh-CN/settings.json'
import zhComponents from '@i18n/locales/zh-CN/components.json'
import zhTriggers from '@i18n/locales/zh-CN/triggers.json'
import enCommon from '@i18n/locales/en/common.json'
import enDashboard from '@i18n/locales/en/dashboard.json'
import enModules from '@i18n/locales/en/modules.json'
import enModels from '@i18n/locales/en/models.json'
import enPacks from '@i18n/locales/en/packs.json'
import enPipeline from '@i18n/locales/en/pipeline.json'
import enRun from '@i18n/locales/en/run.json'
import enTasks from '@i18n/locales/en/tasks.json'
import enSettings from '@i18n/locales/en/settings.json'
import enComponents from '@i18n/locales/en/components.json'
import enTriggers from '@i18n/locales/en/triggers.json'

/** 前端支持的语言码 */
export type AppLanguage = 'zh-CN' | 'en'

export const SUPPORTED_LANGUAGES: AppLanguage[] = ['zh-CN', 'en']

/** 默认语言（服务器配置缺失、localStorage 无缓存时的兜底值） */
export const DEFAULT_LANGUAGE: AppLanguage = 'zh-CN'

/** 本地缓存键：仅用于防首屏闪烁，全局唯一真源是服务器 config.general.language */
const STORAGE_KEY = 'ep-language'

const resources = {
  'zh-CN': {
    common: zhCommon,
    dashboard: zhDashboard,
    modules: zhModules,
    models: zhModels,
    packs: zhPacks,
    pipeline: zhPipeline,
    run: zhRun,
    tasks: zhTasks,
    settings: zhSettings,
    components: zhComponents,
    triggers: zhTriggers,
  },
  en: {
    common: enCommon,
    dashboard: enDashboard,
    modules: enModules,
    models: enModels,
    packs: enPacks,
    pipeline: enPipeline,
    run: enRun,
    tasks: enTasks,
    settings: enSettings,
    components: enComponents,
    triggers: enTriggers,
  },
} as const

/**
 * 语言码归一化：zh* → zh-CN，en* → en，其余/缺失 → zh-CN。
 * 兼容服务器历史值（如 "zh"）与浏览器风格标签（如 "zh-CN"、"en-US"）。
 */
export function normalizeLanguage(s: string | null | undefined): AppLanguage {
  const lower = (s ?? '').trim().toLowerCase()
  if (lower.startsWith('zh')) return 'zh-CN'
  if (lower.startsWith('en')) return 'en'
  return DEFAULT_LANGUAGE
}

/** 读取本地缓存的语言（缺失时回退默认语言） */
function cachedLanguage(): AppLanguage {
  try {
    return normalizeLanguage(localStorage.getItem(STORAGE_KEY))
  } catch {
    // 隐私模式等场景下 localStorage 不可用
    return DEFAULT_LANGUAGE
  }
}

void i18n.use(initReactI18next).init({
  resources,
  lng: cachedLanguage(),
  fallbackLng: DEFAULT_LANGUAGE,
  defaultNS: 'common',
  // 翻译文件为扁平键（如 "action.confirm"），禁用点号嵌套解析，
  // 与 ep-core::i18n 的键查找语义保持一致
  keySeparator: false,
  interpolation: {
    // React 已默认转义，i18next 无需二次转义
    escapeValue: false,
  },
})

/**
 * 应用语言切换：i18next 生效 + localStorage 缓存 + <html lang> + document.title。
 * 注意：本地缓存只是快照，全局真源是服务器配置，调用方应同步 PUT /api/config。
 */
export function setAppLanguage(lang: AppLanguage): void {
  void i18n.changeLanguage(lang)
  try {
    localStorage.setItem(STORAGE_KEY, lang)
  } catch {
    // 忽略存储失败（如隐私模式）
  }
  document.documentElement.lang = lang
  // 标题文案取自 common（品牌名不翻译，缺键时兜底 "EntryPoint"）
  document.title = i18n.t('common:appTitle', { defaultValue: 'EntryPoint' })
}

export default i18n
