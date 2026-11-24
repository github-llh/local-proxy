import { useEffect, useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import Popover from "./views/Popover";
import Settings from "./views/Settings";

type View = "popover" | "settings";

export default function App() {
  const [view, setView] = useState<View>("popover");

  useEffect(() => {
    // 按窗口标签渲染对应视图
    const label = getCurrentWebviewWindow().label;
    if (label === "settings") {
      setView("settings");
    } else {
      setView("popover");
    }
    // 开发模式兜底：URL hash 指定视图
    if (location.hash === "#settings") setView("settings");
  }, []);

  return view === "settings" ? <Settings /> : <Popover />;
}
