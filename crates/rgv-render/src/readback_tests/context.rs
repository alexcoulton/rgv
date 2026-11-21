use super::{
    FORMAT, Oracle, rectangle::paint_rectangle, rectangle::shape_gallery, render_image, test_gpu,
};
use crate::{
    ContextSceneGpuCamera, ContextSceneGpuLaneKey, ContextSceneGpuLaneRequest,
    ContextSceneGpuRenderer, ContextSceneGpuScissor, project_context_scene_rectangle,
};

fn camera(
    target_exact_start: f64,
    target_bases_per_pixel: f64,
    scissor: ContextSceneGpuScissor,
) -> ContextSceneGpuCamera {
    ContextSceneGpuCamera {
        source_exact_start: 1_000.0,
        source_bases_per_pixel: 2.0,
        target_exact_start,
        target_bases_per_pixel,
        panel_left: 12.0,
        frame_top: 9.0,
        vertical_scroll: 4.0,
        scissor,
        selected_object_id: Some(5),
        hovered_object_id: Some(6),
    }
}

#[test]
fn context_scene_pixels_match_the_cpu_oracle_under_pan_zoom_and_scissor() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let size = [320_u32, 200];
    let rectangles = shape_gallery();
    let key = ContextSceneGpuLaneKey::new(1, 1);
    let mut renderer = ContextSceneGpuRenderer::new(&gpu.device, FORMAT);
    let full = ContextSceneGpuScissor {
        x: 0,
        y: 0,
        width: size[0],
        height: size[1],
    };
    let clipped = ContextSceneGpuScissor {
        x: 20,
        y: 15,
        width: 180,
        height: 110,
    };
    let cameras = [
        // Identity scale, fractional pan.
        camera(1_000.0 - 7.25, 2.0, full),
        // Zoomed in 1.6x with a scissor: only the camera transform changes.
        camera(1_013.5, 1.25, clipped),
        // Zoomed out 3x: features narrower than a point keep the one-point minimum width.
        camera(940.0, 6.0, full),
    ];
    for (index, camera) in cameras.into_iter().enumerate() {
        let request = ContextSceneGpuLaneRequest {
            key,
            content_revision: 1,
            camera,
            rectangles: &rectangles,
            retained: None,
            coherence_group: None,
        };
        let report = if index == 0 {
            renderer.prepare(
                &gpu.device,
                &gpu.queue,
                [size[0] as f32, size[1] as f32],
                &[request],
                1 << 20,
            )
        } else {
            // Later cameras are uniform-only captures over the resident geometry.
            renderer
                .capture_camera_frame(&gpu.queue, [size[0] as f32, size[1] as f32], &[request])
                .unwrap()
        };
        assert!(report.frame.contains(key, 1));
        let image = render_image(&gpu, size, |pass| {
            renderer.render(pass, &report.frame);
            Ok::<(), std::convert::Infallible>(())
        });
        let mut oracle = Oracle::new(size);
        let scissor = [
            camera.scissor.x,
            camera.scissor.y,
            camera.scissor.width,
            camera.scissor.height,
        ];
        for rectangle in &rectangles {
            let projected = project_context_scene_rectangle(*rectangle, camera).unwrap();
            paint_rectangle(
                &mut oracle,
                projected,
                1.0,
                scissor,
                camera.selected_object_id,
                camera.hovered_object_id,
            );
        }
        let comparison = oracle.assert_matches(&image, &format!("context scene camera {index}"));
        eprintln!(
            "context scene camera {index} on {}: {} pixels compared, {} painted, {} boundary \
             pixels excluded",
            gpu.description, comparison.compared, comparison.painted, comparison.uncertain
        );
    }
}
