// Headless Meta Quest build (OpenXR, Android):
//
//   Unity -batchmode -nographics -quit -projectPath unity -buildTarget Android \
//         -executeMethod VrFire.EditorTools.VrFireQuest.Setup
//   Unity -batchmode -nographics -quit -projectPath unity -buildTarget Android \
//         -executeMethod VrFire.EditorTools.VrFireQuest.Build
//
// Two invocations because the Input System switch needs an Editor restart (see Setup).
//
// ImportTiles       copies ../bench/tiles/lod1/*.fbx (30 m tiles: ~31k triangles each) into
//                   Assets/VrFire/TilesLod1 (the 10 m desktop tiles are too heavy for a Quest)
// ConfigureAndroid  IL2CPP, ARM64, Vulkan, linear colour, API 32+, ASTC, both input systems
// EnableOpenXR      OpenXR loader for Android, Meta Quest support, Oculus Touch profile,
//                   single-pass instanced (multiview) stereo
// BuildScene        Quest.unity: XR rig with head tracking and thumbstick movement, tiles with
//                   mesh colliders so the rig follows the ground
// BuildApk          Build/Quest/vr_fire_quest.apk (install: adb install -r <apk>)
//
// Options: -tiles <dir with lod1 .fbx>  -scene <scene.json>  -out <apk path>

using System.IO;
using UnityEditor;
using UnityEditor.Build;
using UnityEditor.Build.Reporting;
using UnityEditor.SceneManagement;
using UnityEditor.XR.Management;
using UnityEditor.XR.Management.Metadata;
using UnityEngine;
using UnityEngine.InputSystem;
using UnityEngine.InputSystem.XR;
using UnityEngine.Rendering;
using UnityEngine.XR.Management;
using UnityEngine.XR.OpenXR;
using UnityEngine.XR.OpenXR.Features.Interactions;
using UnityEngine.XR.OpenXR.Features.MetaQuestSupport;

namespace VrFire.EditorTools
{
    public static class VrFireQuest
    {
        const string TilesDir = "Assets/VrFire/TilesLod1";
        const string ScenePath = "Assets/VrFire/Scenes/Quest.unity";
        const string AppId = "dev.chilos.vrfire";

        public static void ImportTiles()
        {
            var src = VrFireBatch.Arg("-tiles", Path.Combine(VrFireBatch.RepoRoot, "bench", "tiles", "lod1"));
            var files = Directory.Exists(src) ? Directory.GetFiles(src, "*.fbx") : new string[0];
            if (files.Length == 0) VrFireBatch.Fail($"no .fbx tiles in {src}; run `vr_fire bake --format fbx --out bench/tiles`");
            Directory.CreateDirectory(TilesDir);
            foreach (var f in files) File.Copy(f, Path.Combine(TilesDir, Path.GetFileName(f)), true);
            AssetDatabase.Refresh(ImportAssetOptions.ForceSynchronousImport);
            Debug.Log($"VRFIRE imported {files.Length} Quest tiles from {src}");
        }

        public static void ConfigureAndroid()
        {
            var android = NamedBuildTarget.Android;
            PlayerSettings.SetApplicationIdentifier(android, AppId);
            PlayerSettings.productName = "vr_fire";
            PlayerSettings.companyName = "vr_fire";
            PlayerSettings.SetScriptingBackend(android, ScriptingImplementation.IL2CPP);
            PlayerSettings.Android.targetArchitectures = AndroidArchitecture.ARM64;
            PlayerSettings.Android.minSdkVersion = AndroidSdkVersions.AndroidApiLevel32;
            PlayerSettings.Android.targetSdkVersion = AndroidSdkVersions.AndroidApiLevelAuto;
            PlayerSettings.SetUseDefaultGraphicsAPIs(BuildTarget.Android, false);
            PlayerSettings.SetGraphicsAPIs(BuildTarget.Android, new[] { GraphicsDeviceType.Vulkan });
            PlayerSettings.colorSpace = ColorSpace.Linear;
            PlayerSettings.defaultInterfaceOrientation = UIOrientation.LandscapeLeft;
            EditorUserBuildSettings.androidBuildSubtarget = MobileTextureSubtarget.ASTC;
            // The rig reads the controllers through the Input System; keep the old manager too.
            var project = new SerializedObject(AssetDatabase.LoadAllAssetsAtPath("ProjectSettings/ProjectSettings.asset")[0]);
            var handler = project.FindProperty("activeInputHandler");
            if (handler != null)
            {
                handler.intValue = 2; // Both
                project.ApplyModifiedPropertiesWithoutUndo();
            }
            Debug.Log("VRFIRE Android player settings: IL2CPP ARM64, Vulkan, linear, API 32+");
        }

        public static void EnableOpenXR()
        {
            VrFireBatch.PrepareXR();
            var group = BuildTargetGroup.Android;
            EditorBuildSettings.TryGetConfigObject(XRGeneralSettings.k_SettingsKey, out XRGeneralSettingsPerBuildTarget perTarget);
            if (perTarget == null)
                perTarget = AssetDatabase.LoadAssetAtPath<XRGeneralSettingsPerBuildTarget>("Assets/XR/XRGeneralSettingsPerBuildTarget.asset");
            if (perTarget == null) VrFireBatch.Fail("XR Plug-in Management settings not found");
            if (!perTarget.HasSettingsForBuildTarget(group)) perTarget.CreateDefaultSettingsForBuildTarget(group);
            if (!perTarget.HasManagerSettingsForBuildTarget(group)) perTarget.CreateDefaultManagerSettingsForBuildTarget(group);
            var general = perTarget.SettingsForBuildTarget(group);
            general.InitManagerOnStart = true;
            if (!XRPackageMetadataStore.AssignLoader(general.Manager, typeof(OpenXRLoader).FullName, group)
                && !general.Manager.activeLoaders.Count.Equals(1))
                VrFireBatch.Fail("could not assign the OpenXR loader for Android");

            // Feature objects for a build target are created lazily; make sure they exist first.
            UnityEditor.XR.OpenXR.Features.FeatureHelpers.RefreshFeatures(group);
            var openxr = OpenXRSettings.GetSettingsForBuildTargetGroup(group);
            if (openxr == null) VrFireBatch.Fail("OpenXR settings for Android not found");
            openxr.renderMode = OpenXRSettings.RenderMode.SinglePassInstanced;
            var quest = openxr.GetFeature<MetaQuestFeature>();
            var touch = openxr.GetFeature<OculusTouchControllerProfile>();
            if (quest == null || touch == null) VrFireBatch.Fail("Meta Quest feature or Oculus Touch profile missing from OpenXR");
            quest.enabled = true;
            touch.enabled = true;
            EditorUtility.SetDirty(quest);
            EditorUtility.SetDirty(touch);
            EditorUtility.SetDirty(openxr);
            EditorUtility.SetDirty(general);
            EditorUtility.SetDirty(general.Manager);
            AssetDatabase.SaveAssets();
            var loaders = string.Join(", ", general.Manager.activeLoaders);
            Debug.Log($"VRFIRE OpenXR for Android: loaders [{loaders}], Meta Quest + Oculus Touch enabled, single-pass instanced");
        }

        public static void BuildScene()
        {
            var json = File.ReadAllText(VrFireBatch.Arg("-scene", Path.Combine(VrFireBatch.RepoRoot, "bench", "scene.json")));
            var scene = JsonUtility.FromJson<QuestSceneFile>(json);
            var urp = VrFireBatch.MakePipeline();
            urp.renderScale = 1f;

            var s = EditorSceneManager.NewScene(NewSceneSetup.EmptyScene, NewSceneMode.Single);
            RenderSettings.ambientMode = AmbientMode.Flat;
            RenderSettings.ambientLight = new Color(0.45f, 0.47f, 0.5f);
            var sun = new GameObject("Sun").AddComponent<Light>();
            sun.type = LightType.Directional;
            sun.intensity = 1.2f;
            sun.shadows = LightShadows.None;
            sun.transform.rotation = Quaternion.Euler(50f, -30f, 0f);

            var mat = AssetDatabase.LoadAssetAtPath<Material>("Assets/VrFire/Settings/Terrain.mat");
            if (mat == null)
            {
                mat = new Material(Shader.Find("Universal Render Pipeline/Lit")) { name = "Terrain" };
                mat.SetColor("_BaseColor", new Color(0.42f, 0.45f, 0.33f));
                mat.SetFloat("_Smoothness", 0.1f);
                AssetDatabase.CreateAsset(mat, "Assets/VrFire/Settings/Terrain.mat");
            }
            foreach (var t in scene.tiles)
            {
                var model = AssetDatabase.LoadAssetAtPath<GameObject>($"{TilesDir}/{t.name}.fbx");
                if (model == null) VrFireBatch.Fail($"Quest tile {t.name} is not imported");
                var go = (GameObject)PrefabUtility.InstantiatePrefab(model);
                go.transform.position = new Vector3(t.east_m, 0f, -t.south_m);
                foreach (var mf in go.GetComponentsInChildren<MeshFilter>())
                {
                    var r = mf.GetComponent<MeshRenderer>();
                    r.sharedMaterial = mat;
                    r.shadowCastingMode = ShadowCastingMode.Off;
                    r.receiveShadows = false;
                    mf.gameObject.AddComponent<MeshCollider>().sharedMesh = mf.sharedMesh;
                }
            }

            // XR rig: origin on the ground at the block centre; the camera is the headset.
            var o = scene.orbit;
            var rig = new GameObject("XR Rig");
            rig.transform.position = new Vector3(o.centre_east_m, o.look_at_height_m, -o.centre_south_m);
            var camGo = new GameObject("Head") { tag = "MainCamera" };
            camGo.transform.SetParent(rig.transform, false);
            var cam = camGo.AddComponent<Camera>();
            cam.nearClipPlane = 0.05f;
            cam.farClipPlane = 30000f;
            cam.clearFlags = CameraClearFlags.SolidColor;
            cam.backgroundColor = new Color(0.55f, 0.7f, 0.9f);
            var tpd = camGo.AddComponent<TrackedPoseDriver>();
            tpd.trackingType = TrackedPoseDriver.TrackingType.RotationAndPosition;
            tpd.positionInput = new InputActionProperty(new InputAction("Head Position", InputActionType.Value, "<XRHMD>/centerEyePosition", expectedControlType: "Vector3"));
            tpd.rotationInput = new InputActionProperty(new InputAction("Head Rotation", InputActionType.Value, "<XRHMD>/centerEyeRotation", expectedControlType: "Quaternion"));
            tpd.trackingStateInput = new InputActionProperty(new InputAction("Head Tracking State", InputActionType.Value, "<XRHMD>/trackingState", expectedControlType: "Integer"));
            var move = rig.AddComponent<QuestRig>();
            move.head = camGo.transform;

            Directory.CreateDirectory(Path.GetDirectoryName(ScenePath));
            EditorSceneManager.SaveScene(s, ScenePath);
            AssetDatabase.SaveAssets();
            Debug.Log($"VRFIRE Quest scene {ScenePath}: {scene.tiles.Length} tiles at 30 m, XR rig at the centre");
        }

        public static void BuildApk()
        {
            var output = VrFireBatch.Arg("-out", Path.Combine(Application.dataPath, "..", "Build", "Quest", "vr_fire_quest.apk"));
            Directory.CreateDirectory(Path.GetDirectoryName(output));
            var report = BuildPipeline.BuildPlayer(new BuildPlayerOptions
            {
                scenes = new[] { ScenePath },
                locationPathName = output,
                target = BuildTarget.Android,
                targetGroup = BuildTargetGroup.Android,
                options = BuildOptions.None,
            });
            var sum = report.summary;
            if (sum.result != BuildResult.Succeeded) VrFireBatch.Fail($"Quest build {sum.result}: {sum.totalErrors} errors");
            Debug.Log($"VRFIRE built {output} ({new FileInfo(output).Length / 1e6:0.0} MB) in {sum.totalTime.TotalSeconds:0}s");
        }

        /// First invocation: everything but the build. Switching the Input System on only
        /// reaches the Editor's compiled scripts after a restart; building in the same session
        /// fails with "script class layout is incompatible between the editor and the player".
        public static void Setup()
        {
            if (EditorUserBuildSettings.activeBuildTarget != BuildTarget.Android)
                VrFireBatch.Fail("start Unity with -buildTarget Android");
            ImportTiles();
            ConfigureAndroid();
            EnableOpenXR();
            BuildScene();
        }

        /// Second invocation (fresh Editor): re-apply the XR settings and build the APK.
        public static void Build()
        {
            if (EditorUserBuildSettings.activeBuildTarget != BuildTarget.Android)
                VrFireBatch.Fail("start Unity with -buildTarget Android");
            EnableOpenXR();
            BuildApk();
        }

        [System.Serializable]
        class QuestTile
        {
            public string name;
            public float east_m, south_m;
        }

        [System.Serializable]
        class QuestOrbit
        {
            public float centre_east_m, centre_south_m, look_at_height_m;
        }

        [System.Serializable]
        class QuestSceneFile
        {
            public QuestTile[] tiles;
            public QuestOrbit orbit;
        }
    }
}
