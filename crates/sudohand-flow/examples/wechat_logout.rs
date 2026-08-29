//! Drive WeChat logout as a generic react workflow. Run with:
//!   SUH_BIN=./target/debug/suh cargo run -p sudohand-flow --example wechat_logout
//! Actions dispatch through $SUH_BIN; VLM location/judgement are the
//! `desktop locate` / `desktop ask` actions. Logs you out.

use sudohand_flow::{CliDispatch, Cond, Report, Runner, Step, Vars, Workflow};

const APP: &str = "com.tencent.xinWeChat";

fn workflow() -> Workflow {
    Workflow::new("wechat-logout")
        .describe("Log out of WeChat via Settings, reacting to the confirm dialog")
        .var("app")
        .step(Step::run("activate", ["desktop", "activate", "--bundle", "{{app}}"]))
        .step(Step::run("open_settings", ["desktop", "key", "--keys", "cmd+,"]))
        .step(Step::run("settle1", ["shell", "run", "--", "sleep", "1"]))
        .step(
            Step::run(
                "find_logout",
                ["desktop", "locate", "--bundle", "{{app}}", "--window-title", "Settings",
                 "--find", "右上角 My Account 区域里的 Log Out 退出登录按钮"],
            )
            .bind("lo"),
        )
        // first click focuses the Settings window, second click presses the button
        .step(Step::run("focus", ["desktop", "click", "--x", "{{lo.point.x}}", "--y", "{{lo.point.y}}"]))
        .step(Step::run("settle2", ["shell", "run", "--", "sleep", "1"]))
        .step(Step::run("click_logout", ["desktop", "click", "--x", "{{lo.point.x}}", "--y", "{{lo.point.y}}"]))
        .step(Step::run("settle3", ["shell", "run", "--", "sleep", "1"]))
        // REACT: did a confirm dialog appear?
        .step(
            Step::run(
                "ask_dialog",
                ["desktop", "ask", "--bundle", "{{app}}", "--window-title", "Settings",
                 "--question", "屏幕上是否出现了“Log out?”确认对话框(有 Cancel 和一个绿色 OK 按钮)"],
            )
            .bind("dlg"),
        )
        .step(Step::branch("has_dialog", Cond::truthy("dlg.yes")).then("find_ok").els("verify"))
        .step(
            Step::run(
                "find_ok",
                ["desktop", "locate", "--bundle", "{{app}}", "--window-title", "Settings",
                 "--find", "确认对话框里那个绿色的 OK 按钮"],
            )
            .bind("ok"),
        )
        .step(Step::run("click_ok", ["desktop", "click", "--x", "{{ok.point.x}}", "--y", "{{ok.point.y}}"]))
        .step(Step::run("settle4", ["shell", "run", "--", "sleep", "2"]))
        // REACT: confirm we actually logged out
        .step(
            Step::run(
                "verify",
                ["desktop", "ask", "--bundle", "{{app}}",
                 "--question", "微信现在是否已登出(显示带 Enter Weixin / Switch Account 的账号登录页,而不是聊天界面)"],
            )
            .bind("st"),
        )
        .step(Step::branch("done?", Cond::truthy("st.yes")).then("done").els("failed"))
        .step(Step::end("done"))
        .step(Step::run("failed", ["shell", "run", "--", "sh", "-c", "echo 'still logged in' >&2; exit 1"]))
}

fn main() {
    let report: Report = Runner::new(CliDispatch::default())
        .run(&workflow(), Vars::from_pairs([("app", APP)]))
        .expect("run");
    for s in &report.steps {
        let mark = if s.ok { "ok  " } else { "FAIL" };
        let extra = s
            .result
            .as_ref()
            .and_then(|r| r.get("yes").or_else(|| r.get("point")))
            .map(|v| format!("  {v}"))
            .unwrap_or_default();
        eprintln!("[{mark}] {}{}", s.id, extra);
    }
    println!(
        "{}",
        serde_json::json!({"ok": report.ok, "cancelled": report.cancelled,
                           "steps": report.steps.len()})
    );
}
