use eframe::egui;
use ep_core::types::TaskStatus;

pub struct TaskEntry {
    pub id: String,
    pub pipeline_name: String,
    pub status: TaskStatus,
    pub elapsed: String,
}

pub fn show(ui: &mut egui::Ui, tasks: &[TaskEntry]) {
    ui.heading("任务中心");
    ui.add_space(8.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("tasks_grid")
            .striped(true)
            .min_col_width(100.0)
            .show(ui, |ui| {
                ui.strong("ID");
                ui.strong("管线");
                ui.strong("状态");
                ui.strong("耗时");
                ui.end_row();

                for t in tasks {
                    ui.label(&t.id);
                    ui.label(&t.pipeline_name);
                    ui.label(task_status_text(&t.status));
                    ui.label(&t.elapsed);
                    ui.end_row();
                }
            });
    });
}

fn task_status_text(s: &TaskStatus) -> String {
    match s {
        TaskStatus::Pending => "等待中".into(),
        TaskStatus::Running => "运行中".into(),
        TaskStatus::Completed => "已完成".into(),
        TaskStatus::Failed(e) => format!("失败: {e}"),
        TaskStatus::Cancelled => "已取消".into(),
    }
}
