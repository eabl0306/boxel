//! The widgets: [`Painter::button`](crate::Painter::button),
//! [`Painter::cycle`](crate::Painter::cycle), [`Painter::toggle`](crate::Painter::toggle) and
//! [`Painter::slider`](crate::Painter::slider). Labels, rectangles, sprites, the dim and
//! tooltips are on [`crate::Painter`] and [`crate::Resp`].

mod button;
mod cycle;
mod slider;

pub use slider::Steps;
