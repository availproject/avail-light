pub struct Attributes {
	pub role: String,
}

impl Attributes {
	pub fn new() -> Self {
		Self {
			role: "crawler".into(),
		}
	}
}
