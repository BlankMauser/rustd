pub trait Shape {
    fn area(&self) -> u32;
    fn name(&self) -> &'static str {
        "shape"
    }
}

pub struct Square(pub u32);
pub struct Circle(pub u32);

impl Shape for Square {
    fn area(&self) -> u32 {
        self.0 * self.0
    }
}

impl Shape for Circle {
    fn area(&self) -> u32 {
        3 * self.0 * self.0
    }
}

pub fn all() -> Vec<Box<dyn Shape>> {
    vec![Box::new(Square(2)), Box::new(Circle(1))]
}

pub fn total(shapes: &[Box<dyn Shape>]) -> u32 {
    shapes.iter().map(|it| it.area()).sum()
}

pub fn first_area() -> u32 {
    let shapes = all();
    shapes[0].area()
}
