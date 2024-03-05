use color_eyre::eyre::Result;

/// Contract for all types that can be decoded from T
pub trait Decode<T>: Sized {
	fn decode(source: T) -> Result<Self>;
}

/// Contract for all types that can be encoded into T
pub trait Encode<T> {
	fn encode(&self) -> Result<T>;
}

// pub trait Database: Send + Sync {
// 	fn put(&self, column_family: Option<&str>, key: &[u8], value: &[u8]) -> Result<()>;
// 	fn get(&self, column_family: Option<&str>, key: &[u8]) -> Result<Option<Vec<u8>>>;
// 	fn delete(&self, column_family: Option<&str>, key: &[u8]) -> Result<()>;
// }

pub trait Database {
	/// Associative type of the database key derived from the custom key mapping.
	type Key;

	/// Associative type of the result of the database query.
	type Result;

	/// Function used to store Values into the Database for the given Key.
	/// Key is encoded into required Database Key, value is encoded into type that is supported by the Database.
	fn put<K, T>(&self, key: K, value: T) -> Result<()>
	where
		K: Into<Self::Key>,
		T: Encode<Self::Result>;

	/// Function used for fetching Value from the Database for the given Key.
	/// Key is encoded into required Database Key, value is encoded into type that is supported by the Database.
	fn get<K, T>(&self, key: K) -> Result<Option<T>>
	where
		K: Into<Self::Key>,
		T: Decode<Self::Result>;

	/// Function used to delete a Value from the Database for the given Key.
	fn delete<K>(&self, key: K) -> Result<()>
	where
		K: Into<Self::Key>;
}
