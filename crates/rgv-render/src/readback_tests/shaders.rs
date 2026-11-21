use wgpu::naga;

use super::test_gpu;
use crate::{gpu_export::resolve_now, shaders::ALL};

/// Parses and validates every composed shader with naga, the compiler wgpu itself uses. This
/// needs no adapter, so a broken shared chunk fails everywhere, not only on machines with a GPU.
#[test]
fn every_composed_shader_parses_and_validates() {
    for shader in ALL {
        let source = shader.source();
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|error| panic!("{}: {}", shader.label, error.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{}: {}", shader.label, error.emit_to_string(&source)));
        let entry_points = module
            .entry_points
            .iter()
            .map(|entry| (entry.stage, entry.name.as_str()))
            .collect::<Vec<_>>();
        assert!(
            entry_points
                .iter()
                .any(|(stage, _)| *stage == naga::ShaderStage::Vertex),
            "{}: no vertex entry point",
            shader.label
        );
        assert!(
            entry_points.contains(&(naga::ShaderStage::Fragment, "fragment_main")),
            "{}: no fragment entry point",
            shader.label
        );
    }
}

/// The shared chunks are defined once: a pipeline file that redeclared one of their functions
/// would be a redefinition error, and one that stopped using them would silently fork the rule.
#[test]
fn shared_wgsl_rules_have_a_single_definition() {
    let definitions = |source: &str, name: &str| source.matches(&format!("fn {name}(")).count();
    for shader in ALL {
        let source = shader.source();
        for name in ["quad_corner", "points_to_clip", "project_genomic_x"] {
            assert_eq!(definitions(&source, name), 1, "{}: {name}", shader.label);
        }
        assert!(source.contains("points_to_clip("), "{}", shader.label);
    }
    for shader in [crate::shaders::RECTANGLE, crate::shaders::CONTEXT_SCENE] {
        let source = shader.source();
        assert_eq!(definitions(&source, "rectangle_fragment_color"), 1);
        assert_eq!(source.matches("rectangle_fragment_color(").count(), 2);
    }
    for shader in [
        crate::shaders::ALIGNMENT_SCENE,
        crate::shaders::ALIGNMENT_SUMMARY,
    ] {
        let source = shader.source();
        assert_eq!(definitions(&source, "rectangle_fragment_color"), 0);
        // One call site, in the pipeline's `project_x`, plus the shared definition.
        assert_eq!(
            source.matches("project_genomic_x(").count(),
            2,
            "{}",
            shader.label
        );
        // No float cancellation helpers may come back; see `shaders/common.wgsl`.
        assert!(!source.contains("two_sum"), "{}", shader.label);
    }
}

/// Compiles every composed shader on the device under an error scope, which also exercises the
/// backend's own translation (MSL, HLSL, SPIR-V or GLSL).
#[test]
fn every_composed_shader_is_accepted_by_the_device() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    for shader in ALL {
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = shader.create_module(&gpu.device);
        let error = resolve_now(scope.pop());
        assert!(error.is_none(), "{}: {}", shader.label, error.unwrap());
        let info = pollster::block_on(module.get_compilation_info());
        assert!(
            info.messages
                .iter()
                .all(|message| message.message_type != wgpu::CompilationMessageType::Error),
            "{}: {:?}",
            shader.label,
            info.messages
        );
    }
}
