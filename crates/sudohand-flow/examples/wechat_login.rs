//! Log back in to WeChat (Auto Login on: "Enter Weixin" needs no QR) as a
//! generic react workflow. Run with:
//!   SUH_BIN=./target/debug/suh cargo run -p sudohand-flow --example wechat_login

use sudohand_flow::{CliDispatch, Cond, Report, Runner, Step, Vars, Workflow};

const APP: &str = "com.tencent.xinWeChat";

fn workflow() -> Workflow {
    Workflow::new("wechat-login")
        .describe("Enter WeChat from the account login page, retrying until logged in")
        .var("app")
        .step(Step::run(
            "activate",
            ["desktop", "activate", "--bundle", "{{app}}"],
        ))
        // loop target
        .step(
            Step::run(
                "find_enter",
                [
                    "desktop",
                    "locate",
                    "--bundle",
                    "{{app}}",
                    "--find",
                    "居中的绿色“Enter Weixin”登录按钮",
                ],
            )
            .bind("btn"),
        )
        .step(Step::run(
            "focus",
            [
                "desktop",
                "click",
                "--x",
                "{{btn.point.x}}",
                "--y",
                "{{btn.point.y}}",
            ],
        ))
        .step(Step::run("settle1", ["shell", "run", "--", "sleep", "1"]))
        .step(Step::run(
            "click_enter",
            [
                "desktop",
                "click",
                "--x",
                "{{btn.point.x}}",
                "--y",
                "{{btn.point.y}}",
            ],
        ))
        .step(Step::run("settle2", ["shell", "run", "--", "sleep", "3"]))
        // REACT: are we in yet? loop back to find_enter if not.
        .step(
            Step::run(
                "verify",
                [
                    "desktop",
                    "ask",
                    "--bundle",
                    "{{app}}",
                    "--question",
                    "微信是否已经登录进入(显示聊天列表/聊天界面,而不再是带 Enter Weixin 的登录页)",
                ],
            )
            .bind("st"),
        )
        .step(
            Step::branch("in?", Cond::truthy("st.yes"))
                .then("done")
                .els("find_enter"),
        )
        .step(Step::end("done"))
}

fn main() {
    let report: Report = Runner::new(CliDispatch::default())
        .max_steps(24) // cap retries (no phone QR fallback here)
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
        serde_json::json!({"ok": report.ok, "steps": report.steps.len()})
    );
}
