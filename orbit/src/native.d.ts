import type { Controller } from "./controller";
import type { Platform } from "./platform";
declare global {
  const wx: Platform & {
    navigateTo(options: { url: string }): void;
    navigateBack(): void;
    showModal(options: { title: string; content: string; confirmText: string; success(result: { confirm: boolean }): void }): void;
  };
  function App(value: { identity?: Controller; onLaunch(): void; onShow(): void; onHide(): void }): void;
  function getApp(): { identity?: Controller };
  function Page<T>(value: T & ThisType<T & { setData(value: Record<string, unknown>): void }>): void;
  const ORBIT_API_ORIGIN: string;
}
