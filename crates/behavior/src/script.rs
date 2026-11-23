use crate::{Decision, Frame};
use anyhow::{Result, ensure};
use rhai::{AST, CallFnOptions, Engine, Scope};

pub struct Script {
    engine: Engine,
    ast: AST,
    deadline: std::sync::Arc<std::sync::Mutex<std::time::Instant>>,
}
impl Script {
    pub fn compile(source: &str) -> Result<Self> {
        ensure!(source.len() <= 64 * 1024, "Script too large");
        let mut engine = Engine::new();
        engine.set_max_operations(20_000);
        engine.set_max_variables(128);
        engine.set_max_functions(32);
        let deadline = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
        let limit = deadline.clone();
        engine.on_progress(move |_| {
            (std::time::Instant::now() > *limit.lock().unwrap())
                .then(|| rhai::Dynamic::from("Script time budget exceeded"))
        });
        engine.set_max_call_levels(16);
        engine.set_max_expr_depths(32, 32);
        engine.set_max_string_size(16 * 1024);
        engine.set_max_array_size(256);
        engine.set_max_map_size(128);
        engine.set_max_modules(0);
        for name in ["eval", "import", "export", "print", "debug"] {
            engine.disable_symbol(name);
        }
        let ast = engine.compile(source).map_err(|e| anyhow::anyhow!("{e}"))?;
        // Top-level code is never evaluated. Only the defined handler runs.
        ensure!(
            ast.iter_functions()
                .any(|f| f.name == "on_event" && f.params.len() == 2),
            "Define on_event(event, state)"
        );
        Ok(Self {
            engine,
            ast,
            deadline,
        })
    }
    pub fn call(&mut self, frame: &Frame, state: &serde_json::Value) -> Result<Decision> {
        let event = rhai::serde::to_dynamic(frame)?;
        let state = rhai::serde::to_dynamic(state)?;
        *self.deadline.lock().unwrap() =
            std::time::Instant::now() + std::time::Duration::from_millis(50);
        let result = self
            .engine
            .call_fn_with_options::<rhai::Dynamic>(
                CallFnOptions::new().eval_ast(false),
                &mut Scope::new(),
                &self.ast,
                "on_event",
                (event, state),
            )
            .map_err(|error| match *error {
                rhai::EvalAltResult::ErrorTerminated(_, _) => {
                    anyhow::anyhow!("脚本执行超过 50 毫秒时间预算")
                }
                error => anyhow::anyhow!("{error}"),
            })?;
        rhai::serde::from_dynamic(&result).map_err(|e| anyhow::anyhow!("{e}"))
    }
}
