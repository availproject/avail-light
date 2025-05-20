use warp::{Filter, Rejection, Reply};

// Define a custom filter type to use consistently across the API
pub type BoxedAnyFilter = warp::filters::BoxedFilter<(Box<dyn Reply>,)>;

/// Create a boxed filter that returns a Box<dyn Reply>
pub fn with_boxed_reply<F, T>(filter: F) -> BoxedAnyFilter
where
	F: Filter<Extract = (T,), Error = Rejection> + Clone + Send + Sync + 'static,
	T: Reply + 'static,
{
	filter
		.map(|reply| Box::new(reply) as Box<dyn Reply>)
		.boxed()
}
