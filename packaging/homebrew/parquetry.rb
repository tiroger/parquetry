# Homebrew cask for Parquetry: template rendered by scripts/update-cask.sh into
# Casks/parquetry.rb of the @GITHUB_OWNER@/homebrew-tap repository.
#   brew install --cask @GITHUB_OWNER@/tap/parquetry
cask "parquetry" do
  version "@VERSION@"
  sha256 "@SHA256@"

  url "https://github.com/@GITHUB_OWNER@/parquetry/releases/download/v#{version}/Parquetry-#{version}.zip"
  name "Parquetry"
  desc "Fast viewer for Parquet, Arrow, CSV and JSON files, local or on S3"
  homepage "https://github.com/@GITHUB_OWNER@/parquetry"

  livecheck do
    url :url
    strategy :github_latest
  end

  auto_updates true # Sparkle
  depends_on arch: :arm64 # Apple Silicon builds only, for now
  depends_on macos: :ventura # macOS 13 or newer

  app "Parquetry.app"
  binary "#{appdir}/Parquetry.app/Contents/Resources/bin/parquetry"

  zap trash: [
    "~/Library/Application Support/Parquetry",
    "~/Library/Caches/Parquetry",
    "~/Library/Caches/io.parquetry.app",
    "~/Library/HTTPStorages/io.parquetry.app",
    "~/Library/Preferences/io.parquetry.app.plist",
    "~/Library/Saved Application State/io.parquetry.app.savedState",
  ]
end
