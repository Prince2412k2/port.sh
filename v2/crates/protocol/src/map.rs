use serde::{Deserialize,Serialize};
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct SearchResult {pub id:String,pub name:String,pub lon:f64,pub lat:f64}
impl SearchResult {pub fn valid(&self)->bool{self.id.len()<=128&&self.name.len()<=256&&self.lon.is_finite()&&self.lat.is_finite()&&(-180.0..=180.0).contains(&self.lon)&&(-85.0..=85.0).contains(&self.lat)}}
