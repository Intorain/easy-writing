import { defineConfig, ConfigEnv, UserConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { resolve } from 'path'
import { existsSync, readdirSync } from 'fs'
import AutoImport from 'unplugin-auto-import/vite'
import Components from 'unplugin-vue-components/vite'
import { ElementPlusResolver } from 'unplugin-vue-components/resolvers'

// 纯本地应用：没有接口地址/代理/环境区分，不读任何 .env 文件。
// 端口约定：web 开发 6789（package.json dev 脚本传参）、桌面 dev UI 4414（tauri 配置引用）。
const WEB_DEV_PORT = 6789

// Element Plus 组件样式是 unplugin-vue-components 在编译每个 .vue 时才注入的 import，
// Vite 启动扫描看不到它们：每进一个没去过的页面就会"发现新依赖 → 重新预构建 → 整页刷新"，
// 刷新前后懒加载的路由块请求会 504（Outdated Optimize Dep），表现为点进页面白屏、又弹回原页，
// 再点一次才正常。这里把全部组件样式入口一次性列进预构建，开发期不再有中途重建。
const elementPlusStyleEntries = () => {
  const dir = resolve(__dirname, 'node_modules/element-plus/es/components')
  if (!existsSync(dir)) return []
  return readdirSync(dir)
    .filter(name => existsSync(resolve(dir, name, 'style/index.mjs')))
    .map(name => `element-plus/es/components/${name}/style/index`)
}

// https://vitejs.dev/config/
export default defineConfig(({ command }: ConfigEnv): UserConfig => {
  return {
    clearScreen: false,
    plugins: [
      vue(),
      AutoImport({
        resolvers: [ElementPlusResolver()],
        imports: ['vue', 'vue-router', 'pinia'],
        // dts 是开发期类型文件，构建期不重写（共享盘上并发回写会 EIO 打断打包）
        dts: command === 'serve' ? 'src/auto-imports.d.ts' : false,
      }),
      Components({
        resolvers: [ElementPlusResolver({
          importStyle: 'sass',
        })],
        dts: command === 'serve' ? 'src/components.d.ts' : false,
      }),
    ],
    resolve: {
      alias: {
        '@': resolve(__dirname, 'src'),
      },
    },
    css: {
      preprocessorOptions: {
        scss: {
          additionalData: `@use "@/styles/element-theme.scss" as *; @use "@/styles/container-breakpoints.scss" as *;`,
          // 现代 Sass API，消除弃用警告。装有 sass-embedded（原生 Dart 进程）时 Vite 会自动优先用它：
          // additionalData 让每个样式块都重算一遍 Element Plus 变量，纯 JS 版 Sass 冷启动要 5 秒多，
          // 原生版约 1 秒（2026-10-10 实测），dev 白屏时间主要就差在这里。
          api: 'modern-compiler',
        },
      },
    },
    server: {
      port: WEB_DEV_PORT,
      strictPort: Boolean(process.env.TAURI_ENV_PLATFORM),
      host: process.env.TAURI_DEV_HOST || false,
      cors: true,
      watch: {
        ignored: ['**/src-tauri/**'],
      },
      // 写作页模块图最大，首次点进去要等 Vite 逐个转译；启动后就在后台先编译好这几页
      warmup: {
        clientFiles: ['./src/views/Writing/index.vue', './src/views/MyBooks/index.vue', './src/views/WorkflowBook/index.vue'],
      },
    },
    optimizeDeps: {
      // 启动时把所有页面和组件扫一遍：页面独有的依赖（vue-flow、echarts、qrcode 等）也提前预构建，
      // 不等用户点进去才发现
      entries: ['index.html', 'src/views/**/*.vue', 'src/components/**/*.vue'],
      include: ['element-plus/es', ...elementPlusStyleEntries()],
    },
    // 生产构建剔除调试日志（保留 console.error/warn 便于线上问题定位）；dev serve 与 debug 构建不剔除
    esbuild:
      command === 'build' && !process.env.TAURI_ENV_DEBUG
        ? { pure: ['console.log', 'console.info', 'console.debug'], drop: ['debugger'] }
        : {},
    build: {
      target: process.env.TAURI_ENV_PLATFORM === 'windows' ? 'chrome105' : 'safari13',
      minify: process.env.TAURI_ENV_DEBUG ? false : 'esbuild',
      sourcemap: Boolean(process.env.TAURI_ENV_DEBUG),
      rollupOptions: {
        output: {
          // 三个体积大户单独分包：降低主包体积、提升缓存命中（库版本不变则 chunk 不变）
          manualChunks(id: string) {
            if (!id.includes('node_modules')) return undefined;
            if (id.includes('element-plus') || id.includes('@element-plus')) return 'element-plus';
            if (id.includes('echarts') || id.includes('zrender')) return 'echarts';
            if (id.includes('@tiptap') || id.includes('prosemirror')) return 'tiptap';
            // 注意：不要给 @vue-flow/@vueuse/d3 做强制分组——与入口链共享依赖时会
            // 造成入口↔分组块循环初始化，生产启动直接 TDZ 白屏（2026-08-30 实测）。
            return undefined;
          },
        },
      },
    },
  }
})
