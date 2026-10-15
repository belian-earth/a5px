# make a hex logo: the mouth of the Amazon in a colour-infrared Sentinel-2
# composite, read with a5px and drawn with a5view::geom_a5()
library(a5px)
library(a5view)
library(ggplot2)

# ----- read bands -----

# Earth Genome's 2023 Sentinel-2 mosaic, tile 22MEE, one COG per band.
# Near-infrared, red and green make a colour-infrared composite: vegetation
# red, sediment-laden water teal. `separate = TRUE` reads the three files
# as columns of one table. A5 resolution 15 (about 180 m cells) over a box
# a little wider than the 42 km frame below.
# Source: https://source.coop/earthgenome/sentinel2-temporal-mosaics,
# CC-BY 4.0. Contains modified Copernicus Sentinel data 2023.

centre <- c(-50.80, -0.60)
half <- 0.22
bbox <- c(
  centre[1] - half,
  centre[2] - half,
  centre[1] + half,
  centre[2] + half
)

tile <- paste0(
  "https://data.source.coop/earthgenome/sentinel2-temporal-mosaics/",
  "22MEE_2023-01-01_2024-01-01/"
)
cir <- a5_read_raster(
  paste0(tile, c("B08", "B04", "B03"), ".tif"),
  resolution = 15L,
  bbox = bbox,
  separate = TRUE,
  io_concurrency = 32L
)

# ----- mix resolutions -----

# Three rings of cell size around the centre: resolution 15 inside, 13 in
# the middle, 11 outside. Each ring's disk is a spherical cap around the
# centre's cell at the resolution of the ring outside it, so its edge
# follows whole cells of that ring; a5_grid_disk(cell, k) gives a lumpier
# disk, k cell steps wide. A cell is in a disk when its supercell is.
res <- c(inner = 15L, middle = 13L, outer = 11L)
radius <- c(inner = 11000, middle = 16000) # metres

disk <- function(r, radius) {
  cap <- a5_spherical_cap(a5_lonlat_to_cell(centre[1], centre[2], r), radius)
  a5_uncompact(cap, r)
}
in_disk <- function(cell, r, radius) {
  vctrs::vec_in(a5_cell_to_supercell(cell, r), disk(r, radius))
}

mid <- a5_aggregate(cir, res[["middle"]])
out <- a5_aggregate(cir, res[["outer"]])

# Drawn coarse to fine: supercells nest by centre, not by outline, so each
# finer disk leaves slivers at its edge that the coarser layer fills.
cirb <- rbind(
  out,
  mid[in_disk(mid$cell, res[["outer"]], radius[["middle"]]), ],
  cir[in_disk(cir$cell, res[["middle"]], radius[["inner"]]), ]
)


# ----- colours -----

# Slightly desaturated and darkened so the label stays legible.
hsv_mat <- rgb2hsv(col2rgb(format(cells_rgb(cirb$B08, cirb$B04, cirb$B03))))
cirb$rgb <- hsv(hsv_mat[1, ], pmin(hsv_mat[2, ] * 0.9, 1), hsv_mat[3, ] * 0.8)

# ----- make plot -----

ggp <- ggplot(cirb, aes(cell = cell, fill = rgb)) +
  scale_fill_identity() +
  geom_a5(colour = "#74ac9071", linewidth = 0.25, render = "vector") +
  annotate(
    "text",
    x = 0,
    y = 0,
    label = "a5px",
    size = 20,
    colour = "#ffffffff",
    family = "Manjari Thin"
  ) +
  coord_sf(
    crs = sprintf("+proj=ortho +lon_0=%f +lat_0=%f", centre[1], centre[2]),
    default_crs = NULL,
    expand = FALSE,
    xlim = c(-21000, 21000),
    ylim = c(-21000, 21000),
    label_axes = "----"
  ) +
  theme_void() +
  theme(plot.background = element_rect(fill = "black", colour = NA))

ggp

tf <- fs::file_temp(ext = "png")
ggsave(tf, ggp, width = 100, height = 100, units = "mm", dpi = 300)

t2 <- fs::file_temp(ext = "png")
p <- cropcircles::crop_hex(
  tf,
  to = t2,
  border_size = 10,
  border_colour = "#74ac90ff",
  bg_fill = "#000000ff"
)

usethis::use_logo(t2)
