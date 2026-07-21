//! UI 页面 — Wave 1 Agent D / Wave 2 Agent H 实现

use eframe::egui;

pub fn dashboard(ui: &mut egui::Ui) {
    ui.heading("仪表盘");
    ui.label("计算设备状态、模块概览、最近任务");
}

pub fn modules(ui: &mut egui::Ui) {
    ui.heading("模块管理");
    ui.label("模块列表、启停控制、模型下载");
}

pub fn pipeline_editor(ui: &mut egui::Ui) {
    ui.heading("管线编辑器");
    ui.label("DAG 节点画布");
}

pub fn tasks(ui: &mut egui::Ui) {
    ui.heading("任务中心");
    ui.label("管线任务列表、进度、日志");
}

pub fn settings(ui: &mut egui::Ui) {
    ui.heading("设置");
    ui.label("计算设备、端口、模型目录、Python 路径");
}
