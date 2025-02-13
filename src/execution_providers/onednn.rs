use super::ExecutionProvider;
use crate::{Error, ExecutionProviderDispatch, Result, SessionBuilder};

#[cfg(all(not(feature = "load-dynamic"), feature = "onednn"))]
extern "C" {
	pub(crate) fn OrtSessionOptionsAppendExecutionProvider_Dnnl(
		options: *mut ort_sys::OrtSessionOptions,
		use_arena: std::os::raw::c_int
	) -> ort_sys::OrtStatusPtr;
}

#[derive(Debug, Default, Clone)]
pub struct OneDNNExecutionProvider {
	use_arena: bool
}

impl OneDNNExecutionProvider {
	pub fn with_arena_allocator(mut self) -> Self {
		self.use_arena = true;
		self
	}

	pub fn build(self) -> ExecutionProviderDispatch {
		self.into()
	}
}

impl From<OneDNNExecutionProvider> for ExecutionProviderDispatch {
	fn from(value: OneDNNExecutionProvider) -> Self {
		ExecutionProviderDispatch::OneDNN(value)
	}
}

impl ExecutionProvider for OneDNNExecutionProvider {
	fn as_str(&self) -> &'static str {
		"DnnlExecutionProvider"
	}

	#[allow(unused, unreachable_code)]
	fn register(&self, session_builder: &SessionBuilder) -> Result<()> {
		#[cfg(any(feature = "load-dynamic", feature = "onednn"))]
		{
			super::get_ep_register!(OrtSessionOptionsAppendExecutionProvider_Dnnl(options: *mut ort_sys::OrtSessionOptions, use_arena: std::os::raw::c_int) -> ort_sys::OrtStatusPtr);
			return crate::error::status_to_result(unsafe {
				OrtSessionOptionsAppendExecutionProvider_Dnnl(session_builder.session_options_ptr, self.use_arena.into())
			})
			.map_err(Error::ExecutionProvider);
		}

		Err(Error::ExecutionProviderNotRegistered(self.as_str()))
	}
}
