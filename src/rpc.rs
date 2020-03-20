use delorean::delorean::Bucket;
use delorean::delorean::{
    delorean_server::Delorean,
    read_response::{
        frame::Data, DataType, FloatPointsFrame, Frame, IntegerPointsFrame, SeriesFrame,
    },
    storage_server::Storage,
    CapabilitiesResponse, CreateBucketRequest, CreateBucketResponse, DeleteBucketRequest,
    DeleteBucketResponse, GetBucketsResponse, Organization, Predicate, ReadFilterRequest,
    ReadGroupRequest, ReadResponse, ReadSource, StringValuesResponse, TagKeysRequest,
    TagValuesRequest, TimestampRange,
};
use delorean::storage::database::Database;
use delorean::storage::SeriesDataType;

use std::convert::TryInto;
use std::sync::Arc;

use tokio::sync::mpsc;
use tonic::Status;

use crate::App;

pub struct GrpcServer {
    pub app: Arc<App>,
}

#[tonic::async_trait]
impl Delorean for GrpcServer {
    async fn create_bucket(
        &self,
        _req: tonic::Request<CreateBucketRequest>,
    ) -> Result<tonic::Response<CreateBucketResponse>, Status> {
        Ok(tonic::Response::new(CreateBucketResponse {}))
    }

    async fn delete_bucket(
        &self,
        _req: tonic::Request<DeleteBucketRequest>,
    ) -> Result<tonic::Response<DeleteBucketResponse>, Status> {
        Ok(tonic::Response::new(DeleteBucketResponse {}))
    }

    async fn get_buckets(
        &self,
        _req: tonic::Request<Organization>,
    ) -> Result<tonic::Response<GetBucketsResponse>, Status> {
        Ok(tonic::Response::new(GetBucketsResponse { buckets: vec![] }))
    }
}

/// This trait implements extraction of information from all storage gRPC requests. The only method
/// required to implement is `read_source_field` because for some requests the field is named
/// `read_source` and for others it is `tags_source`.
trait GrpcInputs {
    fn read_source_field(&self) -> Option<&prost_types::Any>;

    fn read_source_raw(&self) -> Result<&prost_types::Any, Status> {
        Ok(self
            .read_source_field()
            .ok_or_else(|| Status::invalid_argument("missing read_source"))?)
    }

    fn read_source(&self) -> Result<ReadSource, Status> {
        let raw = self.read_source_raw()?;
        let val = &raw.value[..];
        Ok(prost::Message::decode(val).map_err(|_| {
            Status::invalid_argument("value could not be parsed as a ReadSource message")
        })?)
    }

    fn org_id(&self) -> Result<u32, Status> {
        Ok(self
            .read_source()?
            .org_id
            .try_into()
            .map_err(|_| Status::invalid_argument("org_id did not fit in a u32"))?)
    }

    fn bucket(&self, db: &Database) -> Result<Arc<Bucket>, Status> {
        let bucket_id = self
            .read_source()?
            .bucket_id
            .try_into()
            .map_err(|_| Status::invalid_argument("bucket_id did not fit in a u32"))?;

        let maybe_bucket = db
            .get_bucket_by_id(bucket_id)
            .map_err(|_| Status::internal("could not query for bucket"))?;

        Ok(maybe_bucket
            .ok_or_else(|| Status::not_found(&format!("bucket {} not found", bucket_id)))?)
    }
}

impl GrpcInputs for ReadFilterRequest {
    fn read_source_field(&self) -> Option<&prost_types::Any> {
        self.read_source.as_ref()
    }
}

impl GrpcInputs for ReadGroupRequest {
    fn read_source_field(&self) -> Option<&prost_types::Any> {
        self.read_source.as_ref()
    }
}

impl GrpcInputs for TagKeysRequest {
    fn read_source_field(&self) -> Option<&prost_types::Any> {
        self.tags_source.as_ref()
    }
}

impl GrpcInputs for TagValuesRequest {
    fn read_source_field(&self) -> Option<&prost_types::Any> {
        self.tags_source.as_ref()
    }
}

#[tonic::async_trait]
impl Storage for GrpcServer {
    type ReadFilterStream = mpsc::Receiver<Result<ReadResponse, Status>>;

    async fn read_filter(
        &self,
        req: tonic::Request<ReadFilterRequest>,
    ) -> Result<tonic::Response<Self::ReadFilterStream>, Status> {
        let (mut tx, rx) = mpsc::channel(4);

        let read_filter_request = req.into_inner();

        let _org_id = read_filter_request.org_id()?;
        let bucket = read_filter_request.bucket(&self.app.db)?;
        let predicate = read_filter_request.predicate;
        let range = read_filter_request.range;

        let app = Arc::clone(&self.app);

        // TODO: is this blocking because of the blocking calls to the database...?
        tokio::spawn(async move {
            let predicate = predicate.as_ref();
            // TODO: The call to read_series_matching_predicate_and_range takes an optional range,
            // but read_f64_range requires a range-- should this route require a range or use a
            // default or something else?
            let range = range.as_ref().expect("TODO: Must have a range?");

            if let Err(e) = send_series_filters(tx.clone(), app, &bucket, predicate, &range).await {
                tx.send(Err(e)).await.unwrap();
            }
        });

        Ok(tonic::Response::new(rx))
    }

    type ReadGroupStream = mpsc::Receiver<Result<ReadResponse, Status>>;

    async fn read_group(
        &self,
        _req: tonic::Request<ReadGroupRequest>,
    ) -> Result<tonic::Response<Self::ReadGroupStream>, Status> {
        Err(Status::unimplemented("Not yet implemented"))
    }

    type TagKeysStream = mpsc::Receiver<Result<StringValuesResponse, Status>>;

    async fn tag_keys(
        &self,
        req: tonic::Request<TagKeysRequest>,
    ) -> Result<tonic::Response<Self::TagKeysStream>, Status> {
        let (mut tx, rx) = mpsc::channel(4);

        let tag_keys_request = req.get_ref();

        let _org_id = tag_keys_request.org_id()?;
        let bucket = tag_keys_request.bucket(&self.app.db)?;
        let predicate = tag_keys_request.predicate.clone();
        let _range = tag_keys_request.range.as_ref();

        let app = self.app.clone();

        tokio::spawn(async move {
            match app.db.get_tag_keys(&bucket, predicate.as_ref()) {
                Err(_) => tx
                    .send(Err(Status::internal("could not query for tag keys")))
                    .await
                    .unwrap(),
                Ok(tag_keys_iter) => {
                    // TODO: Should these be batched? If so, how?
                    let tag_keys: Vec<_> = tag_keys_iter.map(|s| s.into_bytes()).collect();
                    tx.send(Ok(StringValuesResponse { values: tag_keys }))
                        .await
                        .unwrap();
                }
            }
        });

        Ok(tonic::Response::new(rx))
    }

    type TagValuesStream = mpsc::Receiver<Result<StringValuesResponse, Status>>;

    async fn tag_values(
        &self,
        req: tonic::Request<TagValuesRequest>,
    ) -> Result<tonic::Response<Self::TagValuesStream>, Status> {
        let (mut tx, rx) = mpsc::channel(4);

        let tag_values_request = req.get_ref();

        let _org_id = tag_values_request.org_id()?;
        let bucket = tag_values_request.bucket(&self.app.db)?;
        let predicate = tag_values_request.predicate.clone();
        let _range = tag_values_request.range.as_ref();

        let tag_key = tag_values_request.tag_key.clone();

        let app = self.app.clone();

        tokio::spawn(async move {
            match app.db.get_tag_values(&bucket, &tag_key, predicate.as_ref()) {
                Err(_) => tx
                    .send(Err(Status::internal("could not query for tag values")))
                    .await
                    .unwrap(),
                Ok(tag_values_iter) => {
                    // TODO: Should these be batched? If so, how?
                    let tag_values: Vec<_> = tag_values_iter.map(|s| s.into_bytes()).collect();
                    tx.send(Ok(StringValuesResponse { values: tag_values }))
                        .await
                        .unwrap();
                }
            }
        });

        Ok(tonic::Response::new(rx))
    }

    async fn capabilities(
        &self,
        _: tonic::Request<()>,
    ) -> Result<tonic::Response<CapabilitiesResponse>, Status> {
        Err(Status::unimplemented("Not yet implemented"))
    }
}

async fn send_series_filters(
    mut tx: mpsc::Sender<Result<ReadResponse, Status>>,
    app: Arc<App>,
    bucket: &Bucket,
    predicate: Option<&Predicate>,
    range: &TimestampRange,
) -> Result<(), Status> {
    let filter_iter = app
        .db
        .read_series_matching_predicate_and_range(&bucket, predicate, Some(range))
        .map_err(|e| Status::internal(format!("could not query for filters: {}", e)))?;

    for series_filter in filter_iter {
        let tags = series_filter.tags();
        let data_type = match series_filter.series_type {
            SeriesDataType::F64 => DataType::Float,
            SeriesDataType::I64 => DataType::Integer,
        } as _;
        let series = SeriesFrame { data_type, tags };
        let data = Data::Series(series);
        let data = Some(data);
        let frame = Frame { data };
        let frames = vec![frame];
        let series_frame_response_header = Ok(ReadResponse { frames });

        tx.send(series_frame_response_header).await.unwrap();

        // TODO: Should this match https://github.com/influxdata/influxdb/blob/d96f3dc5abb6bb187374caa9e7c7a876b4799bd2/storage/reads/response_writer.go#L21 ?
        const BATCH_SIZE: usize = 1;

        match series_filter.series_type {
            SeriesDataType::F64 => {
                let iter = app
                    .db
                    .read_f64_range(&bucket, &series_filter, &range, BATCH_SIZE)
                    .map_err(|e| {
                        Status::internal(format!("could not query for SeriesFilter data: {}", e))
                    })?;

                let frames = iter
                    .map(|batch| {
                        // TODO: Performance hazard; splitting this vector is non-ideal
                        let (timestamps, values) =
                            batch.into_iter().map(|p| (p.time, p.value)).unzip();
                        let frame = FloatPointsFrame { timestamps, values };
                        let data = Data::FloatPoints(frame);
                        let data = Some(data);
                        Frame { data }
                    })
                    .collect();
                let data_frame_response = Ok(ReadResponse { frames });

                tx.send(data_frame_response).await.unwrap();
            }
            SeriesDataType::I64 => {
                let iter = app
                    .db
                    .read_i64_range(&bucket, &series_filter, &range, BATCH_SIZE)
                    .map_err(|e| {
                        Status::internal(format!("could not query for SeriesFilter data: {}", e))
                    })?;

                let frames = iter
                    .map(|batch| {
                        // TODO: Performance hazard; splitting this vector is non-ideal
                        let (timestamps, values) =
                            batch.into_iter().map(|p| (p.time, p.value)).unzip();
                        let frame = IntegerPointsFrame { timestamps, values };
                        let data = Data::IntegerPoints(frame);
                        let data = Some(data);
                        Frame { data }
                    })
                    .collect();
                let data_frame_response = Ok(ReadResponse { frames });

                tx.send(data_frame_response).await.unwrap();
            }
        }
    }

    Ok(())
}