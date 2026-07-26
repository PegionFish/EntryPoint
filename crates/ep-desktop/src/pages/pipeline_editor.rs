use eframe::egui;

pub fn show(ui: &mut egui::Ui) {
    ui.heading("管线编辑器");
    ui.add_space(4.0);
    ui.label("管线编辑器 — 即将实现");
    ui.add_space(8.0);

    egui::SidePanel::left("pipeline_nodes")
        .default_width(200.0)
        .show_inside(ui, |ui| {
            ui.strong("可用节点");
            ui.separator();
            ui.label("🎙 ASR 转写");
            ui.label("🔊 TTS 合成");
            ui.label("🔇 音频降噪");
            ui.label("📝 OCR 识别");
            ui.label("🖼 图像处理");
            ui.label("🌐 翻译");
        });

    egui::CentralPanel::default()
        .frame(egui::Frame::default().fill(egui::Color32::from_gray(40)))
        .show_inside(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.label(
                    egui::RichText::new("拖拽节点到此处构建管线")
                        .color(egui::Color32::from_gray(140))
                        .size(16.0),
                );
            });
        });
}
