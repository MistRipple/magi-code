/** 站点脚本只识别目标和等待页面状态，所有写入由 Worker 复用既有输入与表单控件语义执行。 */
export const INSTALL_WEB_SETUP_INPUT = String.raw`((id, expression) => {
  const state = {
    id, done:false, value:null, failed:false, pending:null, disposed:false,
    perform(kind, element, value) {
      if (this.disposed || this.pending) return Promise.reject(new Error('browser_setup_input_closed'));
      const ref = globalThis.__magiBrowserAutomation.reference(element);
      return new Promise((resolve, reject) => { this.pending = {kind, ref, value, resolve, reject}; });
    },
    snapshot() {
      const p = this.pending;
      return {done:this.done, value:this.value, failed:this.failed,
        request:p ? {kind:p.kind, ref:p.ref, value:p.value ?? null} : null};
    },
    settle() {
      const p = this.pending;
      this.pending = null;
      p?.resolve();
    },
    dispose() {
      this.disposed = true;
      const p = this.pending;
      this.pending = null;
      p?.reject(new Error('browser_setup_input_closed'));
    },
  };
  globalThis.__magiSetupInput?.dispose();
  globalThis.__magiSetupInput = state;
  Promise.resolve().then(() => (0, eval)(expression)).then(
    value => { state.value = value; state.done = true; },
    () => { state.failed = true; state.done = true; },
  );
  return true;
})`;
