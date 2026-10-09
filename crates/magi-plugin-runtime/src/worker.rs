use crate::protocol::{CapabilityReply, WorkerFrame};
use crate::{ExecutionError, ExecutionErrorCode, Invocation};
use rquickjs::{Context, Ctx, Exception, Function, Module, Object, Runtime, Value};
use std::{
    cell::RefCell,
    io::{self, BufRead, BufReader, Read, Write},
    rc::Rc,
    time::{Duration, Instant},
};

const MAX_INITIAL_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// 私有 stdio Worker；不读取包路径、用户环境或工作区，也不接入任何业务服务。
pub fn run_worker_stdio() -> io::Result<()> {
    let mut input = BufReader::new(io::stdin());
    let mut output = io::stdout();
    let bytes = read_frame(&mut input, MAX_INITIAL_FRAME_BYTES)?;
    let invocation: Invocation = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let result = execute(invocation, input);
    let frame = match result {
        Ok(result) => WorkerFrame::Complete { result },
        Err(error) => WorkerFrame::Failed { error },
    };
    write_frame(&mut output, &frame, MAX_INITIAL_FRAME_BYTES)
}

fn execute(
    invocation: Invocation,
    input: BufReader<io::Stdin>,
) -> Result<serde_json::Value, ExecutionError> {
    invocation.limits.validate()?;
    let runtime = Runtime::new().map_err(|_| resource_error())?;
    runtime.set_memory_limit(invocation.limits.memory_bytes);
    runtime.set_max_stack_size(invocation.limits.stack_bytes);
    let deadline = Instant::now() + Duration::from_millis(invocation.limits.timeout_ms);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    // 不启用 loader、dyn-load、文件、stdio、网络、环境变量或 Node.js 标准库。
    let context = Context::full(&runtime).map_err(|_| resource_error())?;
    let bridge = Rc::new(RefCell::new(Bridge {
        input,
        output: io::stdout(),
        sequence: 0,
        max_frame_bytes: invocation.limits.max_frame_bytes,
        max_calls: invocation.limits.max_capability_calls,
    }));
    context.with(|ctx| {
        let execution = (|| -> rquickjs::Result<serde_json::Value> {
            let callback_bridge = bridge.clone();
            let callback_ctx = ctx.clone();
            let callback =
                Function::new(ctx.clone(), move |operation: String, arguments: String| {
                    callback_bridge
                        .borrow_mut()
                        .call(&callback_ctx, operation, arguments)
                })?;
            let make_sdk: Function = ctx.eval(
                r#"(nativeCall) => {
                const parse = JSON.parse, stringify = JSON.stringify, ErrorType = Error;
                return Object.freeze({ call(operation, payload) {
                    const reply = parse(nativeCall(operation, stringify(payload)));
                    if ('Err' in reply) throw new ErrorType('宿主拒绝插件能力请求');
                    return reply.Ok;
                }});
            }"#,
            )?;
            let sdk: Object = make_sdk.call((callback,))?;
            let module =
                Module::declare(ctx.clone(), "magi-plugin.mjs", invocation.source.as_bytes())?;
            let (module, ready) = module.eval()?;
            ready.finish::<()>()?;
            let handler: Function = module.get("default")?;
            let input = ctx.json_parse(
                serde_json::to_vec(&invocation.input).map_err(|_| rquickjs::Error::Allocation)?,
            )?;
            let result: Value = handler.call((input, sdk))?;
            let result = if let Some(promise) = result.as_promise() {
                promise.finish::<Value>()?
            } else {
                result
            };
            let json = ctx
                .json_stringify(result)?
                .ok_or_else(|| Exception::throw_type(&ctx, "插件必须返回 JSON 值"))?;
            let json = json.to_string()?;
            if json.len() > invocation.limits.max_frame_bytes / 2 {
                return Err(Exception::throw_range(&ctx, "插件结果超过报文上限"));
            }
            serde_json::from_str(&json)
                .map_err(|_| Exception::throw_type(&ctx, "插件必须返回 JSON 值"))
        })();
        execution.map_err(|error| {
            if Instant::now() >= deadline {
                return ExecutionError::new(ExecutionErrorCode::TimedOut, "插件执行超过宿主时限");
            }
            if matches!(error, rquickjs::Error::Allocation) {
                return resource_error();
            }
            let exception = ctx.catch();
            let message = exception
                .as_object()
                .and_then(|e| e.get::<_, String>("message").ok())
                .unwrap_or_default();
            if message.contains("out of memory")
                || message.contains("stack overflow")
                || message.contains("报文上限")
            {
                return resource_error();
            }
            ExecutionError::new(
                ExecutionErrorCode::ScriptFailed,
                "插件脚本执行失败或返回值无效",
            )
        })
    })
}

struct Bridge {
    input: BufReader<io::Stdin>,
    output: io::Stdout,
    sequence: u32,
    max_frame_bytes: usize,
    max_calls: u32,
}

impl Bridge {
    fn call(&mut self, ctx: &Ctx<'_>, operation: String, json: String) -> rquickjs::Result<String> {
        if !crate::host::valid_operation(&operation) || self.sequence >= self.max_calls {
            return Err(Exception::throw_type(ctx, "插件能力请求无效或超出次数上限"));
        }
        self.sequence += 1;
        if json.len() > self.max_frame_bytes / 2 {
            return Err(Exception::throw_range(ctx, "插件能力参数超过报文上限"));
        }
        let arguments = serde_json::from_str(&json)
            .map_err(|_| Exception::throw_type(ctx, "能力参数必须为 JSON"))?;
        let frame = WorkerFrame::Capability {
            sequence: self.sequence,
            operation,
            arguments,
        };
        write_frame(&mut self.output, &frame, self.max_frame_bytes)
            .map_err(|_| Exception::throw_internal(ctx, "能力通道不可用"))?;
        let bytes = read_frame(&mut self.input, self.max_frame_bytes)
            .map_err(|_| Exception::throw_internal(ctx, "能力通道不可用"))?;
        let reply: CapabilityReply = serde_json::from_slice(&bytes)
            .map_err(|_| Exception::throw_internal(ctx, "能力响应无效"))?;
        if reply.sequence != self.sequence {
            return Err(Exception::throw_internal(ctx, "能力响应身份无效"));
        }
        serde_json::to_string(&reply.result).map_err(|_| rquickjs::Error::Allocation)
    }
}

fn read_frame(reader: &mut impl BufRead, max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(max_bytes as u64 + 1)
        .read_until(b'\n', &mut bytes)?;
    if bytes.is_empty() || !bytes.ends_with(b"\n") || bytes.len() > max_bytes {
        return Err(io::Error::other("Worker 报文缺失、被截断或超限"));
    }
    Ok(bytes)
}

fn write_frame(
    writer: &mut impl Write,
    frame: &impl serde::Serialize,
    max_bytes: usize,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(frame).map_err(io::Error::other)?;
    if bytes.len() + 1 > max_bytes {
        return Err(io::Error::other("Worker 报文超限"));
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn resource_error() -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCode::ResourceLimit,
        "插件执行超过内存、栈或报文上限",
    )
}
