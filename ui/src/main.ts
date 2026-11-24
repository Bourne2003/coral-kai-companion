import "./styles/main.css";
import { SoulMateApp } from "./App";

const root = document.getElementById("app");
if (root) {
  const app = new SoulMateApp(root);
  void app.init();
}
