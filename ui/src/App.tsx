import {useEffect, useState} from "react";
import {getCurrentWebviewWindow} from "@tauri-apps/api/webviewWindow";
import {listen} from "@tauri-apps/api/event";
import {api} from "./api";
import {type Lang, LangContext} from "./i18n";
import Popover from "./views/Popover";
import Settings from "./views/Settings";

type View = "popover" | "settings";

function normalizeLang(value: string): Lang {
  return value === "en" ? "en" : "zh-CN";
}

export default function App() {
  const [view, setView] = useState<View>("popover");
  const [lang, setLang] = useState<Lang>("zh-CN");

  useEffect(() => {
    // 按窗口标签渲染对应视图
    const label = getCurrentWebviewWindow().label;
    setView(label === "settings" ? "settings" : "popover");
    // 开发模式兜底：URL hash 指定视图
    if (location.hash === "#settings") setView("settings");
  }, []);

  useEffect(() => {
    // 语言来自配置；设置窗口保存后通过 Tauri 事件广播到所有窗口
    api
      .getConfig()
      .then((config) => setLang(normalizeLang(config.ui.language)))
      .catch(() => {
      });
    let disposed = false;
    let unlisten: (() => void) | undefined;
    listen<string>("language", (event) => setLang(normalizeLang(event.payload)))
      .then((stop) => {
        if (disposed) {
          stop();
        } else {
          unlisten = stop;
        }
      })
      .catch(() => {
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    document.documentElement.lang = lang;
  }, [lang]);

  return (
    <LangContext.Provider value={lang}>
      {view === "settings" ? <Settings/> : <Popover/>}
    </LangContext.Provider>
  );
}
