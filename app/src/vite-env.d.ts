/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** 可选：覆盖 assets 根目录（默认 Ricardo 本机 C:/Users/Qiu/Desktop/Deskpet/assets）。 */
  readonly VITE_DESKPET_ASSETS?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
