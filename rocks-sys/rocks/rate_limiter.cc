
#include "rocksdb/rate_limiter.h"

#include "rocks/ctypes.hpp"


using namespace rocksdb;

using std::shared_ptr;

extern "C" {


rocks_ratelimiter_t* rocks_ratelimiter_create(
    int64_t rate_bytes_per_sec,
    int64_t refill_period_us,
    int32_t fairness) {
  rocks_ratelimiter_t* rate_limiter = new rocks_ratelimiter_t;
  rate_limiter->rep = NewGenericRateLimiter(rate_bytes_per_sec,
                                            refill_period_us, fairness);
  return rate_limiter;
}

void rocks_ratelimiter_destroy(rocks_ratelimiter_t *limiter) {
  if (limiter->rep) {
    delete limiter->rep;
  }
  delete limiter;
}



}