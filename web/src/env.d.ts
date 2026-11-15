declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<object, object, unknown>;
  export default component;
}

interface Realm {
  key: string;
  name: string;
  dark: boolean;
  desc: string;
  swatch: string[];
}
interface ThemeState {
  realm: string;
  follow: boolean;
  light: string;
  dark: string;
  current: string;
}
interface Window {
  JJTheme: {
    get(): ThemeState;
    set(next: Partial<ThemeState>): void;
    REALMS: Realm[];
    onChange(fn: (s: ThemeState) => void): void;
  };
}
